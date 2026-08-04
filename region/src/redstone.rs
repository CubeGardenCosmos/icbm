//! 红石简化逻辑（不包括活塞）。
//!
//! 对照 vanilla `RedStoneWireBlock`/`SignalGetter` 的可观察行为：
//! - 红石线（`minecraft:redstone_wire`，状态 4138..4153 = power 0..15）：
//!   `calculateTargetStrength` 简化——`i` = 6 方向邻居电源信号（红石块/亮火把
//!   15；邻居为红石导体时并入其 6 方向强信号——火把/红石块可穿透实体方块，
//!   即“火把在方块下方、线在方块上方”经典布局），`j` = 线传导（水平邻居线 +
//!   对角上/下：邻居为导体且本线上方非导体 → 取邻居块顶部线；邻居非导体 →
//!   取邻居下方线），结果 `max(i, j - 1)`（对照 `RedStoneWireBlock
//!   .calculateTargetStrength` + `SignalGetter.getSignal`/`getDirectSignalTo`）。
//! - 红石块（9223）输出满强度 15；红石灯（7418 灭 / 7419 亮）：相邻任一信号
//!   ≥ 1 点亮（`hasNeighborSignal`——红石线按原始强度计，不 -1）；
//! - 红石火把（5738 灭 / 5739 亮）：视为始终输出 15 的电源（无方块遮挡/熄灭
//!   逻辑）。
//!
//! 状态 ID 对照 vanilla `Block.BLOCK_STATE_REGISTRY`（master
//! `data/src/generated/block.rs`）。

use std::collections::HashMap;

use icbm_mc_767::packets::play::block_state_ids as ids;

/// 红石电源强度常量。
pub mod power {
    /// 红石块输出强度。
    pub const BLOCK: i32 = 15;
    /// 红石火把输出强度。
    pub const TORCH: i32 = 15;
}

/// 红石线状态：`base + power`（base=4138，power 0..15）。
pub const WIRE_BASE: i32 = 4138;

/// 判断方块是否为红石线，返回当前强度。
#[must_use]
pub const fn wire_power(state_id: i32) -> Option<i32> {
    if state_id >= WIRE_BASE && state_id < WIRE_BASE + 16 {
        Some(state_id - WIRE_BASE)
    } else {
        None
    }
}

/// 红石线指定强度的状态 ID。
#[must_use]
pub const fn wire_state(power: i32) -> i32 {
    let power = if power < 0 {
        0
    } else if power > 15 {
        15
    } else {
        power
    };
    WIRE_BASE + power
}

/// 方块是否为红石电源（红石块 / 亮火把）。
#[must_use]
pub const fn is_power_source(state_id: i32) -> bool {
    state_id == ids::REDSTONE_BLOCK || state_id == ids::REDSTONE_TORCH_ON
}

/// 方块是否为红石相关方块（红石线/灯/火把/红石块；红石 tick 处理目标）。
#[must_use]
pub const fn is_redstone_block(state_id: i32) -> bool {
    wire_power(state_id).is_some()
        || state_id == ids::REDSTONE_LAMP
        || state_id == ids::REDSTONE_LAMP_ON
        || state_id == ids::REDSTONE_TORCH_OFF
        || state_id == ids::REDSTONE_TORCH_ON
        || state_id == ids::REDSTONE_BLOCK
}

/// 电源输出强度（`Block.getSignal` 弱信号；红石线在 `calculateTargetStrength`
/// 计算期间 `shouldSignal=false` 输出 0，见 [`weak_source_signal`]）。
#[must_use]
pub const fn source_power(state_id: i32) -> i32 {
    match state_id {
        ids::REDSTONE_BLOCK => power::BLOCK,
        ids::REDSTONE_TORCH_ON => power::TORCH,
        _ => 0,
    }
}

/// 弱信号（vanilla `SignalGetter.getSignal` 的弱部分）：红石块/亮火把 15；
/// **红石线输出 0**（`calculateTargetStrength` 先置 `shouldSignal=false`，
/// 线不参与 `getBestNeighborSignal`，否则相邻线会以全额强度供电、永远不衰减）。
#[must_use]
pub const fn weak_source_signal(state_id: i32) -> i32 {
    source_power(state_id)
}

/// 是否红石导体（vanilla `isRedstoneConductor` 的 icbm 方块集近似）：完整实体
/// 方块（石头/木板/黑曜石等）可让强信号穿透（火把在下方 → 上方方块顶部的线
/// 通电）；空气、流体、红石线/灯/火把、传送门、作物为非导体。
#[must_use]
pub const fn is_conductor(state_id: i32) -> bool {
    state_id != 0
        && !crate::fluids::is_fluid(state_id)
        && wire_power(state_id).is_none()
        && state_id != ids::REDSTONE_LAMP
        && state_id != ids::REDSTONE_LAMP_ON
        && state_id != ids::REDSTONE_TORCH_OFF
        && state_id != ids::REDSTONE_TORCH_ON
        && state_id != ids::NETHER_PORTAL
        && state_id != ids::NETHER_PORTAL + 1
        && crate::crops::is_crop(state_id).is_none()
}

/// `getDirectSignalTo(blockPos)`：6 方向邻居的强信号最大值（火把/红石块 15，
/// 红石线在 `shouldSignal=false` 期间 0）。用于导体方块穿透传导。
fn strong_signal_around(snapshot: &HashMap<(i32, i32, i32), i32>, x: i32, y: i32, z: i32) -> i32 {
    let mut best = 0;
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        let state = snapshot
            .get(&(x + dx, y + dy, z + dz))
            .copied()
            .unwrap_or(0);
        if is_power_source(state) {
            best = 15;
        }
    }
    best
}

/// `SignalGetter.getSignal(neighbor, direction)` 的 icbm 简化：邻居电源弱信号 +
/// 邻居为导体时并入其 6 方向强信号（穿透）。红石线邻居贡献 0（`shouldSignal`
/// = false，线强度只经 `j` 传导路径计算）。
fn neighbor_signal(snapshot: &HashMap<(i32, i32, i32), i32>, x: i32, y: i32, z: i32) -> i32 {
    let state = snapshot.get(&(x, y, z)).copied().unwrap_or(0);
    let mut sig = weak_source_signal(state);
    if is_conductor(state) {
        sig = sig.max(strong_signal_around(snapshot, x, y, z));
    }
    sig
}

/// `getWireSignal`：红石线原始强度（非线 0）。
fn wire_signal(state_id: i32) -> i32 {
    wire_power(state_id).unwrap_or(0)
}

/// 计算某红石线应接收的强度（vanilla `RedStoneWireBlock.calculateTargetStrength`）：
/// `i` = 6 方向邻居信号（电源 + 导体强信号穿透），`j` = 线传导（水平 + 对角
/// 上/下），结果 `max(i, j - 1)`——电源直接驱动为全额，线传导每格衰减 1
/// （对照 vanilla 红石 1 tick/格的传播速率）。
///
/// `snapshot` 由调用方构建（tick 前快照，含红石脏区块及正交邻区），越界坐标
/// 视为空气。
#[must_use]
#[allow(clippy::implicit_hasher)] // 内部状态快照，hasher 无关紧要
pub fn compute_wire_power(snapshot: &HashMap<(i32, i32, i32), i32>, x: i32, y: i32, z: i32) -> i32 {
    let mut i = 0;
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        i = i.max(neighbor_signal(snapshot, x + dx, y + dy, z + dz));
    }
    let mut j = 0;
    let above_is_conductor = is_conductor(snapshot.get(&(x, y + 1, z)).copied().unwrap_or(0));
    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let nx = x + dx;
        let nz = z + dz;
        let neighbor = snapshot.get(&(nx, y, nz)).copied().unwrap_or(0);
        j = j.max(wire_signal(neighbor));
        if is_conductor(neighbor) && !above_is_conductor {
            // 邻居块为导体且本线上方非导体：取邻居块顶部的线（对角上，
            // “线贴着方块侧面爬上顶部”）
            j = j.max(wire_signal(
                snapshot.get(&(nx, y + 1, nz)).copied().unwrap_or(0),
            ));
        } else if !is_conductor(neighbor) {
            // 邻居非导体：取邻居下方的线（对角下/垂直挂线）
            j = j.max(wire_signal(
                snapshot.get(&(nx, y - 1, nz)).copied().unwrap_or(0),
            ));
        }
    }
    i.max(j.saturating_sub(1))
}

/// 红石灯是否应点亮（vanilla `hasNeighborSignal(lampPos)`）：6 方向邻居任一
/// `getSignal > 0`。与 [`compute_wire_power`] 不同：此处 `shouldSignal=true`，
/// 相邻红石线按**原始强度**计（不 -1）——功率 1 的线也必须点亮灯。
#[must_use]
#[allow(clippy::implicit_hasher)] // 内部状态快照，hasher 无关紧要
pub fn neighbor_has_signal(
    snapshot: &HashMap<(i32, i32, i32), i32>,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    for (dx, dy, dz) in [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ] {
        let nx = x + dx;
        let ny = y + dy;
        let nz = z + dz;
        let state = snapshot.get(&(nx, ny, nz)).copied().unwrap_or(0);
        let mut sig = source_power(state);
        if let Some(p) = wire_power(state) {
            sig = sig.max(p); // 线按全额强度（shouldSignal=true）
        }
        if is_conductor(state) {
            sig = sig.max(strong_signal_around(snapshot, nx, ny, nz));
        }
        if sig > 0 {
            return true;
        }
    }
    false
}

/// 红石灯状态 ID（lit 或 unlit）。
#[must_use]
pub const fn lamp_state(lit: bool) -> i32 {
    if lit {
        ids::REDSTONE_LAMP_ON
    } else {
        ids::REDSTONE_LAMP
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_wire_power, is_conductor, neighbor_has_signal, wire_power, wire_state};
    use icbm_mc_767::packets::play::block_state_ids as ids;
    use std::collections::HashMap;

    fn snapshot_at(
        blocks: &HashMap<(i32, i32, i32), i32>,
        x: i32,
        y: i32,
        z: i32,
    ) -> HashMap<(i32, i32, i32), i32> {
        blocks
            .iter()
            .filter(|&((bx, by, bz), _)| {
                (*bx - x).abs() <= 2 && (*by - y).abs() <= 2 && (*bz - z).abs() <= 2
            })
            .map(|(&pos, &state)| (pos, state))
            .collect()
    }

    #[test]
    fn wire_power_mapping() {
        assert_eq!(wire_power(4138), Some(0));
        assert_eq!(wire_power(4138 + 15), Some(15));
        assert_eq!(wire_power(ids::STONE), None);
        assert_eq!(super::wire_state(5), 4143);
        assert_eq!(super::wire_state(20), 4153); // clamp 到 15
    }

    #[test]
    fn power_source_detection() {
        use super::{is_power_source, source_power};
        assert!(is_power_source(ids::REDSTONE_BLOCK));
        assert!(is_power_source(ids::REDSTONE_TORCH_ON));
        assert!(!is_power_source(ids::REDSTONE_TORCH_OFF));
        assert_eq!(source_power(ids::REDSTONE_BLOCK), 15);
        assert_eq!(source_power(ids::REDSTONE_TORCH_ON), 15);
        // 导体判定：石头/黑曜石是导体，空气/水/红石线/火把/灯不是
        assert!(is_conductor(ids::STONE));
        assert!(is_conductor(ids::OBSIDIAN));
        assert!(!is_conductor(ids::AIR));
        assert!(!is_conductor(ids::WATER));
        assert!(!is_conductor(ids::REDSTONE_WIRE));
        assert!(!is_conductor(ids::REDSTONE_TORCH_ON));
        assert!(!is_conductor(ids::REDSTONE_LAMP));
    }

    #[test]
    fn wire_powered_horizontally_by_block() {
        // 红石块在 (1,0,0)，线在 (0,0,0) → 线强度 15
        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 0), ids::REDSTONE_WIRE);
        blocks.insert((1, 0, 0), ids::REDSTONE_BLOCK);
        blocks.insert((0, -1, 0), ids::STONE); // 线下的支撑
        let snap = snapshot_at(&blocks, 0, 0, 0);
        assert_eq!(compute_wire_power(&snap, 0, 0, 0), 15);
    }

    #[test]
    fn wire_powered_from_above_and_below() {
        // 红石块在 (0,1,0)（线上方）→ 15；红石块在 (0,-1,0)（线压在红石块上）→ 15
        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 0), ids::REDSTONE_WIRE);
        blocks.insert((0, 1, 0), ids::REDSTONE_BLOCK);
        let snap = snapshot_at(&blocks, 0, 0, 0);
        assert_eq!(compute_wire_power(&snap, 0, 0, 0), 15);

        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 0), ids::REDSTONE_WIRE);
        blocks.insert((0, -1, 0), ids::REDSTONE_BLOCK);
        let snap = snapshot_at(&blocks, 0, 0, 0);
        assert_eq!(compute_wire_power(&snap, 0, 0, 0), 15);
    }

    #[test]
    fn torch_under_block_powers_wire_on_top() {
        // 火把 (0,-2,0) → 石头 (0,-1,0) → 线 (0,0,0)：强信号穿透导体
        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 0), ids::REDSTONE_WIRE);
        blocks.insert((0, -1, 0), ids::STONE);
        blocks.insert((0, -2, 0), ids::REDSTONE_TORCH_ON);
        let snap = snapshot_at(&blocks, 0, 0, 0);
        assert_eq!(compute_wire_power(&snap, 0, 0, 0), 15);
    }

    #[test]
    fn wire_decays_one_per_block() {
        // 线 A(0,0,0)=15，线 B(1,0,0)：B 应得 14
        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 0), super::wire_state(15));
        blocks.insert((1, 0, 0), ids::REDSTONE_WIRE);
        blocks.insert((0, -1, 0), ids::STONE);
        blocks.insert((1, -1, 0), ids::STONE);
        let snap = snapshot_at(&blocks, 1, 0, 0);
        assert_eq!(compute_wire_power(&snap, 1, 0, 0), 14);
    }

    #[test]
    fn wire_climbs_up_diagonally() {
        // 红石块 (-1,0,0) → 线 A(0,0,0)=15，石头柱 (1,0,0)，线 B(1,1,0) 在柱顶：
        // B 经对角上传导（A 的水平邻居是导体柱、A 上方非导体 → 取柱顶的线）得 14
        let mut blocks = HashMap::new();
        blocks.insert((-1, 0, 0), ids::REDSTONE_BLOCK);
        blocks.insert((0, 0, 0), super::wire_state(15));
        blocks.insert((1, 0, 0), ids::STONE);
        blocks.insert((1, 1, 0), ids::REDSTONE_WIRE);
        blocks.insert((0, -1, 0), ids::STONE);
        let snap = snapshot_at(&blocks, 0, 0, 0);
        assert_eq!(compute_wire_power(&snap, 0, 0, 0), 15);
        let snap = snapshot_at(&blocks, 1, 1, 0);
        assert_eq!(
            compute_wire_power(&snap, 1, 1, 0),
            14,
            "wire on top gets 14"
        );
    }

    #[test]
    fn lamp_lights_from_wire_power_one() {
        // 灯 (1,0,0) 相邻线 (0,0,0) 功率 1 → 必须点亮（不 -1）
        let mut blocks = HashMap::new();
        blocks.insert((0, 0, 0), super::wire_state(1));
        blocks.insert((1, 0, 0), ids::REDSTONE_LAMP);
        blocks.insert((0, -1, 0), ids::STONE);
        let snap = snapshot_at(&blocks, 1, 0, 0);
        assert!(neighbor_has_signal(&snap, 1, 0, 0));
    }
}
