//! 活塞与 BUD（对照 `PistonBlock`/`PistonHandler` 的简化模型）。
//!
//! - 状态：普通/粘性活塞各 12 态（facing 6 × extended 2），活塞头 24 态
//!   （facing 6 × 子态 4）；状态 ID 按 vanilla `StateManager` 顺序
//!   （EXTENDED 最快）：`base + facing*2 + extended`。
//! - 供电：相邻方块红石强度 ≥ 1（直接连接）**或正上方方块供电**（准连接
//!   QC——活塞能看到头顶的电源变化，这是 BUD 电路的基础）。
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
/// 方向索引 → 偏移（0=下 1=上 2=北 3=南 4=西 5=东）。
#[must_use]
pub const fn facing_offset(facing: usize) -> (i32, i32, i32) {
    match facing {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// 解析活塞状态：返回 `(base, extended)`；非活塞返回 `None`。
#[must_use]
pub const fn piston_info(state_id: i32) -> Option<(i32, bool)> {
    if state_id >= PISTON_BASE && state_id < PISTON_BASE + 12 {
        Some((PISTON_BASE, (state_id - PISTON_BASE) % 2 == 1))
    } else if state_id >= STICKY_PISTON_BASE && state_id < STICKY_PISTON_BASE + 12 {
        Some((STICKY_PISTON_BASE, (state_id - STICKY_PISTON_BASE) % 2 == 1))
    } else {
        None
    }
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

/// 活塞头的朝向索引。
#[must_use]
pub const fn piston_head_facing(state_id: i32) -> usize {
    let f = (state_id - PISTON_HEAD_BASE) / 4;
    (if f < 0 { 0 } else if f > 5 { 5 } else { f }) as usize
}

/// 活塞头状态（`facing` 的子态 0 = 普通）。
#[must_use]
pub const fn piston_head_state(facing: usize) -> i32 {
    PISTON_HEAD_BASE + facing as i32 * 4
}

/// 活塞状态（`extended` 布尔）。
#[must_use]
pub const fn piston_state(base: i32, facing: usize, extended: bool) -> i32 {
    base + facing as i32 * 2 + if extended { 1 } else { 0 }
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
        let Some((base, _)) = piston_info(state) else {
            last_powered.remove(&(x, y, z));
            continue;
        };
        let facing = ((state - base) / 2).clamp(0, 5) as usize;
        let extended = (state - base) % 2 == 1;
        let powered = is_powered(access, x, y, z);
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

/// 活塞是否被供电：相邻方块红石强度 ≥ 1，或正上方方块供电（QC 准连接）。
fn is_powered(access: &mut impl BlockAccess, x: i32, y: i32, z: i32) -> bool {
    let neighbors = [
        (x + 1, y, z),
        (x - 1, y, z),
        (x, y, z + 1),
        (x, y, z - 1),
        (x, y + 1, z),
        (x, y - 1, z),
    ]
    .into_iter()
    .map(|(nx, ny, nz)| access.block_at(nx, ny, nz))
    .collect::<Vec<_>>();
    if crate::redstone::compute_wire_power(neighbors) >= 1 {
        return true;
    }
    // QC：正上方方块是电源（红石块/点亮火把）或红石线有强度
    let above = access.block_at(x, y + 1, z);
    crate::redstone::is_power_source(above) || crate::redstone::wire_power(above) >= Some(1)
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
    let head_pos = chain
        .first()
        .map_or((x + dx, y + dy, z + dz), |(p, _)| *p);
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
    use std::collections::{HashMap, HashSet};

    #[test]
    fn piston_state_math() {
        // facing 0=下 1=上 ... 5=东；extended=false 在前
        assert_eq!(piston_state(PISTON_BASE, 1, false), 2013); // 朝上未伸出
        assert_eq!(piston_state(PISTON_BASE, 1, true), 2014); // 朝上伸出
        assert_eq!(piston_state(STICKY_PISTON_BASE, 0, false), 1992);
        assert_eq!(piston_info(2013), Some((PISTON_BASE, false)));
        assert_eq!(piston_info(2014), Some((PISTON_BASE, true)));
        assert!(is_piston_head(piston_head_state(1)));
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
        // 活塞朝上（facing=1）位于 y=0，前方依次 stone, coral_fan
        let piston_pos = (0, 0, 0);
        blocks.map.insert(piston_pos, piston_state(PISTON_BASE, 1, false));
        blocks.map.insert((0, 1, 0), 1); // stone
        blocks.map.insert((0, 2, 0), 12_843); // coral fan
        blocks.map.insert((1, 0, 0), 9223); // 红石块供电
        let mut last = HashMap::new();
        // 供电已成立 → 上升沿伸出
        let actions = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert_eq!(actions.len(), 1, "piston should extend once");
        // 伸出后布局：y=1 活塞头，y=2 stone，y=3 coral_fan（前移），
        // y=4 珊瑚扇副本（末端刷取）
        assert_eq!(blocks.map.get(&(0, 1, 0)).copied(), Some(piston_head_state(1)));
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
        blocks.map.insert(piston_pos, piston_state(PISTON_BASE, 1, false));
        let mut last = HashMap::new();
        let _ = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert!(!last.get(&piston_pos).copied().unwrap_or(false));
        // QC：正上方放红石块 → 活塞供电（BUD 触发条件）
        blocks.map.insert((5, 6, 5), 9223);
        let _ = tick_pistons(&[piston_pos], &mut last, &mut blocks);
        assert!(last.get(&piston_pos).copied().unwrap_or(false));
    }
}
