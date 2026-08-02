//! 红石简化逻辑（不包括活塞）。
//!
//! 对照 vanilla `RedstoneWireBlock` 的可观察行为简化：
//! - 红石线（`minecraft:redstone_wire`，状态 4138..4153 = power 0..15）：从
//!   相邻电源（红石块、点亮火把）接收信号，并向相邻线传播（强度递减）；
//! - 红石块（`minecraft:redstone_block`，状态 9223）输出满强度 15；
//! - 红石灯（`minecraft:redstone_lamp`，状态 7418 灭 / 7419 亮）：相邻信号
//!   ≥ 1 时点亮，否则熄灭；
//! - 红石火把（`minecraft:redstone_torch`，状态 5738 灭 / 5739 亮）：简化模型
//!   视为始终输出信号的电源（无方块遮挡逻辑）。
//!
//! 状态 ID 对照 vanilla `Block.BLOCK_STATE_REGISTRY`（master
//! `data/src/generated/block.rs`）。

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
    let power = if power < 0 { 0 } else if power > 15 { 15 } else { power };
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

/// 电源输出强度。
#[must_use]
pub const fn source_power(state_id: i32) -> i32 {
    match state_id {
        ids::REDSTONE_BLOCK => power::BLOCK,
        ids::REDSTONE_TORCH_ON => power::TORCH,
        _ => 0,
    }
}

/// 计算某红石线应接收的强度：相邻方块中「电源强度」与「相邻线强度 - 1」的最大值。
#[must_use]
pub fn compute_wire_power(
    neighbors: impl IntoIterator<Item = i32>,
) -> i32 {
    neighbors
        .into_iter()
        .map(|state| {
            if is_power_source(state) {
                source_power(state)
            } else if let Some(p) = wire_power(state) {
                p.saturating_sub(1)
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0)
}

/// 红石灯是否应点亮（相邻信号 ≥ 1）。
#[must_use]
pub const fn lamp_should_light(neighbor_power: i32) -> bool {
    neighbor_power >= 1
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
    use super::{compute_wire_power, lamp_should_light, wire_power, wire_state};
    use icbm_mc_767::packets::play::block_state_ids as ids;

    #[test]
    fn wire_power_mapping() {
        assert_eq!(wire_power(4138), Some(0));
        assert_eq!(wire_power(4138 + 15), Some(15));
        assert_eq!(wire_power(ids::STONE), None);
        assert_eq!(wire_state(5), 4143);
        assert_eq!(wire_state(20), 4153); // clamp 到 15
    }

    #[test]
    fn power_source_detection() {
        use super::{is_power_source, source_power};
        assert!(is_power_source(ids::REDSTONE_BLOCK));
        assert!(is_power_source(ids::REDSTONE_TORCH_ON));
        assert!(!is_power_source(ids::REDSTONE_TORCH_OFF));
        assert_eq!(source_power(ids::REDSTONE_BLOCK), 15);
        assert_eq!(source_power(ids::REDSTONE_TORCH_ON), 15);
    }

    #[test]
    fn wire_power_propagates_with_decay() {
        // 红石块相邻的红石线应得 15；相邻线的相邻线得 14
        let power = compute_wire_power([ids::REDSTONE_BLOCK]);
        assert_eq!(power, 15);
        let next = compute_wire_power([wire_state(15)]);
        assert_eq!(next, 14);
        let empty = compute_wire_power([ids::STONE, ids::AIR]);
        assert_eq!(empty, 0);
    }

    #[test]
    fn lamp_toggle() {
        assert!(lamp_should_light(1));
        assert!(lamp_should_light(15));
        assert!(!lamp_should_light(0));
        assert_eq!(super::lamp_state(true), ids::REDSTONE_LAMP_ON);
        assert_eq!(super::lamp_state(false), ids::REDSTONE_LAMP);
    }
}
