//! 活塞与 BUD（对照 `PistonBlock`/`PistonHandler` 的简化模型）。
//!
//! - 状态：普通/粘性活塞各 12 态、活塞头 24 态，状态布局与属性序严格对照
//!   vanilla 1.21.1 全局 palette（`blocks.json` 报告，协议 767）：
//!   - 活塞体 `id = base + extended*6 + facing`，facing 序
//!     NORTH=0,EAST=1,SOUTH=2,WEST=3,UP=4,DOWN=5（`DirectionalBlock.FACING`）；
//!   - 活塞头 `id = base + facing*4 + short*2 + type`（默认头 = normal + short=false）。
//! - 供电：相邻方块红石强度 ≥ 1（直接连接，**排除 facing 方向**）**或正上方
//!   方块及其邻居供电**（准连接 QC——活塞能看到头顶的电源变化，这是 BUD
//!   电路的基础；对照 vanilla `PistonBaseBlock.getNeighborSignal` 与 Pumpkin
//!   `should_extend`）。
//! - 上升沿：伸出——把前方 ≤12 个可推方块整体前移 1 格，末端放置活塞头，
//!   活塞本体进入 extended 态；粘性活塞缩回时拉回前方 1 格。
//! - 故障特性：推动珊瑚/珊瑚扇/TNT 时**复制一份**（原方块保留 + 副本前移），
//!   复现 vanilla 的珊瑚/TNT 刷取。

use std::collections::HashMap;

use icbm_mc_767::packets::play::block_state_ids as ids;

/// 方块访问抽象（region store / 测试内存地图实现）。
pub trait BlockAccess {
    /// 读方块状态。
    fn block_at(&mut self, x: i32, y: i32, z: i32) -> i32;
    /// 写方块状态。
    fn set_block(&mut self, x: i32, y: i32, z: i32, state: i32);
}

/// 普通活塞状态基址（2011..2022）。
pub const PISTON_BASE: i32 = ids::PISTON_BASE;
/// 粘性活塞状态基址（1992..2003）。
pub const STICKY_PISTON_BASE: i32 = ids::STICKY_PISTON_BASE;
/// 活塞头状态基址（2023..2046，每 facing 4 个子态）。
pub const PISTON_HEAD_BASE: i32 = ids::PISTON_HEAD_BASE;
/// 活塞朝向索引 → 偏移（vanilla `DirectionalBlock.FACING` 序：
/// 0=北 1=东 2=南 3=西 4=上 5=下）。
#[must_use]
pub const fn facing_offset(facing: usize) -> (i32, i32, i32) {
    match facing {
        0 => (0, 0, -1),
        1 => (1, 0, 0),
        2 => (0, 0, 1),
        3 => (-1, 0, 0),
        4 => (0, 1, 0),
        _ => (0, -1, 0),
    }
}

/// 解析活塞状态：返回 `(base, facing 索引, extended)`；非活塞返回 `None`。
///
/// 布局对照 vanilla 1.21.1：`id = base + extended_index*6 + facing`
/// （属性序 `extended[true,false] × facing[north,east,south,west,up,down]`，
/// extended=true 在前，offset 0..5 = true / 6..11 = false）。
#[must_use]
pub const fn piston_info(state_id: i32) -> Option<(i32, usize, bool)> {
    let base = if state_id >= PISTON_BASE && state_id < PISTON_BASE + 12 {
        PISTON_BASE
    } else if state_id >= STICKY_PISTON_BASE && state_id < STICKY_PISTON_BASE + 12 {
        STICKY_PISTON_BASE
    } else {
        return None;
    };
    let offset = state_id - base;
    Some((base, (offset % 6) as usize, offset < 6))
}

/// 是否粘性活塞。
#[must_use]
pub const fn is_sticky(base: i32) -> bool {
    base == STICKY_PISTON_BASE
}

/// 是否活塞头。
#[must_use]
pub const fn is_piston_head(state_id: i32) -> bool {
    state_id >= PISTON_HEAD_BASE && state_id < PISTON_HEAD_BASE + 24
}

/// 活塞头的朝向索引（vanilla：`type × facing × short` 布局，facing 步长 4）。
#[must_use]
pub const fn piston_head_facing(state_id: i32) -> usize {
    let f = (state_id - PISTON_HEAD_BASE) / 4;
    (if f < 0 {
        0
    } else if f > 5 {
        5
    } else {
        f
    }) as usize
}

/// 活塞头状态（normal 类型 + short=false，即 vanilla 默认头；
/// `id = base + facing*4 + short*2 + type`，short=false=1）。
#[must_use]
pub const fn piston_head_state(facing: usize) -> i32 {
    PISTON_HEAD_BASE + facing as i32 * 4 + 2
}

/// 活塞状态（`extended` 布尔；`id = base + extended_index*6 + facing`，
/// extended=true 在属性序前面）。
#[must_use]
pub const fn piston_state(base: i32, facing: usize, extended: bool) -> i32 {
    base + if extended { 0 } else { 6 } + facing as i32
}

/// 可被活塞推动的方块（基岩/黑曜石等不可推；珊瑚/TNT 特殊处理）。
#[must_use]
pub const fn is_pushable(state_id: i32) -> bool {
    state_id != 0
        && state_id != ids::BEDROCK
        && state_id != ids::OBSIDIAN
        && state_id != ids::NETHER_PORTAL
        && !is_piston_head(state_id)
}

/// 是否珊瑚/珊瑚扇（推动时复制一份，vanilla 刷取故障）。
#[must_use]
pub const fn is_coral_fan(state_id: i32) -> bool {
    state_id >= ids::TUBE_CORAL_FAN - 20 && state_id <= ids::TUBE_CORAL_FAN + 8
    // tube(12823)..horn(12851)：珊瑚 + 珊瑚扇
}

/// 活塞 tick 结果：一个伸出/缩回动作。
#[derive(Debug)]
pub struct PistonAction {
    /// 活塞位置。
    pub pos: (i32, i32, i32),
    /// 是否伸出。
    pub extending: bool,
    /// 本体新状态。
    pub new_state: i32,
    /// 需要广播的方块更新（位置 → 状态）。
    pub updates: Vec<((i32, i32, i32), i32)>,
}

/// 计算所有活塞的供电并检测边沿；返回需要执行的动作。
///
/// `block_at`/`set_block` 由调用方（region store）提供；`last_powered` 为上次
/// 供电快照（调用方维护），返回新的快照。
#[allow(clippy::implicit_hasher)] // 内部状态快照，hasher 无关紧要
pub fn tick_pistons(
    pistons: &[(i32, i32, i32)],
    last_powered: &mut HashMap<(i32, i32, i32), bool>,
    access: &mut impl BlockAccess,
) -> Vec<PistonAction> {
    let mut actions = Vec::new();
    for &(x, y, z) in pistons {
        let state = access.block_at(x, y, z);
        let Some((base, facing, extended)) = piston_info(state) else {
            last_powered.remove(&(x, y, z));
            continue;
        };
        let powered = is_powered(access, x, y, z, facing);
        let was_powered = last_powered.get(&(x, y, z)).copied().unwrap_or(false);
        last_powered.insert((x, y, z), powered);
        if powered == was_powered {
            continue;
        }
        if powered && !extended {
            // 上升沿伸出
            if let Some(action) = extend_piston(access, (x, y, z), base, facing) {
                actions.push(action);
            }
        } else if !powered && extended {
            // 下降沿缩回
            actions.push(retract_piston(access, (x, y, z), base, facing));
        }
    }
    actions
}

/// 活塞是否被供电（vanilla `PistonBaseBlock.getNeighborSignal` + Pumpkin
/// `should_extend`）：
/// 1. 6 方向直接邻居除 facing 方向外任一信号 > 0（`hasSignal`，含电源强信号
///    穿透导体与导线 power）；
/// 2. QC 准连接：正上方方块及其 5 个邻居（除下方）任一信号 > 0——活塞能看到
///    头顶的电源变化（BUD 电路基础）。
fn is_powered(access: &mut impl BlockAccess, x: i32, y: i32, z: i32, facing: usize) -> bool {
    use crate::redstone::signal_at;
    // 快照 ±2 范围：电源穿透导体需查两格（火把在下方方块之下）
    let mut snapshot: HashMap<(i32, i32, i32), i32> = HashMap::new();
    for dx in -2..=2 {
        for dy in -2..=2 {
            for dz in -2..=2 {
                let key = (x + dx, y + dy, z + dz);
                snapshot
                    .entry(key)
                    .or_insert_with(|| access.block_at(key.0, key.1, key.2));
            }
        }
    }
    // 1. 直接邻居（排除 facing 方向）
    let (fx, fy, fz) = facing_offset(facing);
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        if (dx, dy, dz) == (fx, fy, fz) {
            continue;
        }
        if signal_at(&snapshot, x + dx, y + dy, z + dz) > 0 {
            return true;
        }
    }
    // 2. QC：正上方方块（pos.above()）的 5 个邻居（排除下方——下方即活塞
    //    自身；vanilla `getNeighborSignal` 第 3 段：活塞能看到头顶方块侧面/顶
    //    面的电源变化，BUD 电路基础）
    for (dx, dy, dz) in [(1, 0, 0), (-1, 0, 0), (0, 1, 0), (0, 0, 1), (0, 0, -1)] {
        if signal_at(&snapshot, x + dx, y + 1 + dy, z + dz) > 0 {
            return true;
        }
    }
    false
}

/// 伸出：把前方 ≤12 个可推方块整体前移 1 格，放置活塞头。
fn extend_piston(
    access: &mut impl BlockAccess,
    (x, y, z): (i32, i32, i32),
    base: i32,
    facing: usize,
) -> Option<PistonAction> {
    let (dx, dy, dz) = facing_offset(facing);
    let mut updates = Vec::new();
    // 收集前方链
    let mut chain: Vec<((i32, i32, i32), i32)> = Vec::new();
    let mut tip = (x + dx, y + dy, z + dz);
    loop {
        let state = access.block_at(tip.0, tip.1, tip.2);
        if state == 0 {
            break;
        }
        if !is_pushable(state) {
            return None; // 推不动
        }
        chain.push((tip, state));
        if chain.len() >= 12 {
            return None;
        }
        tip = (tip.0 + dx, tip.1 + dy, tip.2 + dz);
    }
    // 从链尾向前移动（整体前移 1 格）
    for i in (0..chain.len()).rev() {
        let (pos, state) = chain[i];
        let next = (pos.0 + dx, pos.1 + dy, pos.2 + dz);
        access.set_block(next.0, next.1, next.2, state);
        updates.push((next, state));
    }
    // 故障特性：链中含珊瑚/珊瑚扇/TNT 时，在链末端空位再放一份副本
    // （原链正常前移 + 末端多一个副本 → 珊瑚/TNT 刷取）。
    if chain
        .iter()
        .any(|(_, state)| is_coral_fan(*state) || *state == ids::TNT)
    {
        let (_, dup_state) = *chain.last().expect("non-empty chain");
        let dup_state = if is_coral_fan(dup_state) {
            dup_state
        } else {
            chain
                .iter()
                .rev()
                .find(|(_, s)| is_coral_fan(*s) || *s == ids::TNT)
                .map_or(dup_state, |(_, s)| *s)
        };
        // 副本放在链末端之后的空位（tip 已是链末块前移后的位置）
        let dup_pos = (tip.0 + dx, tip.1 + dy, tip.2 + dz);
        access.set_block(dup_pos.0, dup_pos.1, dup_pos.2, dup_state);
        updates.push((dup_pos, dup_state));
    }
    // 链首置为活塞头
    let head_pos = chain.first().map_or((x + dx, y + dy, z + dz), |(p, _)| *p);
    let head_state = piston_head_state(facing);
    access.set_block(head_pos.0, head_pos.1, head_pos.2, head_state);
    updates.push((head_pos, head_state));
    // 本体进入 extended
    let new_state = piston_state(base, facing, true);
    access.set_block(x, y, z, new_state);
    updates.push(((x, y, z), new_state));
    Some(PistonAction {
        pos: (x, y, z),
        extending: true,
        new_state,
        updates,
    })
}

/// 缩回：移除活塞头；粘性活塞拉回前方 1 格方块。
fn retract_piston(
    access: &mut impl BlockAccess,
    (x, y, z): (i32, i32, i32),
    base: i32,
    facing: usize,
) -> PistonAction {
    let (dx, dy, dz) = facing_offset(facing);
    let mut updates = Vec::new();
    // 移除活塞头
    let head_pos = (x + dx, y + dy, z + dz);
    let head_state = access.block_at(head_pos.0, head_pos.1, head_pos.2);
    if is_piston_head(head_state) {
        access.set_block(head_pos.0, head_pos.1, head_pos.2, 0);
        updates.push((head_pos, 0));
    }
    // 粘性活塞：拉回前方第 2 格方块到头部位置
    if is_sticky(base) {
        let pull_pos = (x + 2 * dx, y + 2 * dy, z + 2 * dz);
        let pulled = access.block_at(pull_pos.0, pull_pos.1, pull_pos.2);
        if pulled != 0 && is_pushable(pulled) {
            access.set_block(head_pos.0, head_pos.1, head_pos.2, pulled);
            updates.push((head_pos, pulled));
            access.set_block(pull_pos.0, pull_pos.1, pull_pos.2, 0);
            updates.push((pull_pos, 0));
        }
    }
    let new_state = piston_state(base, facing, false);
    access.set_block(x, y, z, new_state);
    updates.push(((x, y, z), new_state));
    PistonAction {
        pos: (x, y, z),
        extending: false,
        new_state,
        updates,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PISTON_BASE, STICKY_PISTON_BASE, is_coral_fan, is_piston_head, is_pushable,
        piston_head_state, piston_info, piston_state, tick_pistons,
    };
    use std::collections::HashMap;

    #[test]
    fn piston_state_math() {
        // vanilla 1.21.1 布局：`id = base + extended*6 + facing`，facing 序
        // NORTH=0,EAST=1,SOUTH=2,WEST=3,UP=4,DOWN=5。
        assert_eq!(piston_state(PISTON_BASE, 0, false), 2017); // 默认态（北、未伸出）
        assert_eq!(piston_state(PISTON_BASE, 4, false), 2021); // 朝上未伸出
        assert_eq!(piston_state(PISTON_BASE, 4, true), 2015); // 朝上伸出
        assert_eq!(piston_state(STICKY_PISTON_BASE, 0, false), 1998); // 粘性默认
        assert_eq!(piston_info(2017), Some((PISTON_BASE, 0, false))); // 默认 = 北
        assert_eq!(piston_info(2011), Some((PISTON_BASE, 0, true))); // extended + 北
        assert_eq!(piston_info(2015), Some((PISTON_BASE, 4, true))); // 朝上伸出
        assert_eq!(piston_info(1998), Some((STICKY_PISTON_BASE, 0, false)));
        // 活塞头：默认头 2025 = normal/north/short=false
        assert_eq!(piston_head_state(0), 2025);
        assert!(is_piston_head(piston_head_state(2)));
        assert_eq!(super::piston_head_facing(2025), 0);
    }

    #[test]
    fn facing_offsets_match_vanilla() {
        use super::facing_offset;
        assert_eq!(facing_offset(0), (0, 0, -1)); // 北
        assert_eq!(facing_offset(1), (1, 0, 0)); // 东
        assert_eq!(facing_offset(2), (0, 0, 1)); // 南
        assert_eq!(facing_offset(3), (-1, 0, 0)); // 西
        assert_eq!(facing_offset(4), (0, 1, 0)); // 上
        assert_eq!(facing_offset(5), (0, -1, 0)); // 下
    }

    #[test]
    fn coral_fan_ranges() {
        assert!(is_coral_fan(12_843)); // tube_coral_fan
        assert!(is_coral_fan(12_851)); // horn_coral_fan
        assert!(is_coral_fan(12_823)); // tube_coral
        assert!(!is_coral_fan(2095)); // TNT 单独处理
    }

    #[test]
    fn pushability() {
        assert!(is_pushable(1)); // stone
        assert!(!is_pushable(0)); // air
        assert!(!is_pushable(79)); // bedrock
        assert!(!is_pushable(2354)); // obsidian
    }

    struct MemBlocks {
        map: HashMap<(i32, i32, i32), i32>,
    }
    impl super::BlockAccess for MemBlocks {
        fn block_at(&mut self, x: i32, y: i32, z: i32) -> i32 {
            self.map.get(&(x, y, z)).copied().unwrap_or(0)
        }
        fn set_block(&mut self, x: i32, y: i32, z: i32, state: i32) {
            self.map.insert((x, y, z), state);
        }
    }

    #[test]
    fn piston_extend_pushes_chain_and_duplicates_coral() {
        let mut blocks = MemBlocks {
            map: HashMap::new(),
        };
        // 活塞朝上（facing=4）位于 y=0，前方依次 stone, coral_fan
        let piston_pos = (0, 0, 0);
        blocks
            .map
            .insert(piston_pos, piston_state(PISTON_BASE, 4, false));
        blocks.map.insert((0, 1, 0), 1); // stone
        blocks.map.insert((0, 2, 0), 12_843); // coral fan
        blocks.map.insert((1, 0, 0), 9223); // 红石块供电
        let mut last = HashMap::new();
        // 供电已成立 → 上升沿伸出
        let actions = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert_eq!(actions.len(), 1, "piston should extend once");
        // 伸出后布局：y=1 活塞头（up = 2023+4*4+2 = 2041），y=2 stone，
        // y=3 coral_fan（前移），y=4 珊瑚扇副本（末端刷取）
        assert_eq!(
            blocks.map.get(&(0, 1, 0)).copied(),
            Some(piston_head_state(4))
        );
        assert_eq!(blocks.map.get(&(0, 2, 0)).copied(), Some(1));
        assert_eq!(blocks.map.get(&(0, 3, 0)).copied(), Some(12_843));
        assert_eq!(blocks.map.get(&(0, 4, 0)).copied(), Some(12_843));
    }

    #[test]
    fn piston_bud_via_quasi_connectivity() {
        let mut blocks = MemBlocks {
            map: HashMap::new(),
        };
        let piston_pos = (5, 5, 5);
        blocks
            .map
            .insert(piston_pos, piston_state(PISTON_BASE, 1, false)); // 朝东
        let mut last = HashMap::new();
        let _ = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert!(!last.get(&piston_pos).copied().unwrap_or(false));
        // QC（BUD）：红石块放在正上方方块的东侧 (6,6,5)——不是活塞直接邻居，
        // 只经 `getNeighborSignal` 的 pos.above() 段（vanilla 第 3 段）供电。
        blocks.map.insert((6, 6, 5), 9223);
        let _ = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert!(last.get(&piston_pos).copied().unwrap_or(false));
    }

    #[test]
    fn piston_not_powered_from_facing_direction() {
        // vanilla `getNeighborSignal`：facing 方向的直接邻居不供电（活塞头
        // 伸出空间）；侧方红石块供电。
        let mut blocks = MemBlocks {
            map: HashMap::new(),
        };
        let piston_pos = (5, 5, 5);
        blocks
            .map
            .insert(piston_pos, piston_state(PISTON_BASE, 1, false)); // 朝东
        let mut last = HashMap::new();
        // 前方（东）放红石块：不得供电（facing 方向被排除）
        blocks.map.insert((6, 5, 5), 9223);
        let _ = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert!(!last.get(&piston_pos).copied().unwrap_or(false));
        // 侧方（南）放红石块：供电
        blocks.map.insert((5, 5, 6), 9223);
        let _ = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert!(last.get(&piston_pos).copied().unwrap_or(false));
    }
}
