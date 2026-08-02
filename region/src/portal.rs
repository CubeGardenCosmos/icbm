//! 下界传送门：框架检测与传送门方块生成。
//!
//! 对照 vanilla `NetherPortalBlock`/`PortalForcer` 的可观察行为简化：
//! - 黑曜石框架：4×5 竖直框架（宽 4 格、高 5 格，四角与两列用黑曜石）；
//! - 玩家在黑曜石柱上点火/放置时若构成完整框架，内部 4×3 空间填充
//!   `nether_portal` 方块（axis=x 或 z，取决于框架朝向）；
//! - 传送门方块放置后，玩家站入即触发维度切换（由 region 服务处理）。
//!
//! 本模块只做纯几何检测：输入 `block_at` 查询，输出待填充的传送门方块列表。

use icbm_mc_767::packets::play::block_state_ids;

/// 框架朝向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalAxis {
    /// 南北走向（沿 Z），传送门方块 axis=x。
    X,
    /// 东西走向（沿 X），传送门方块 axis=z。
    Z,
}

/// 检测以 (x, z) 为左下角黑曜石柱的 4×5 框架。
///
/// 返回填充内部所需的方块列表。框架：宽 4 格（沿主方向）、高 5 格；两列
/// 黑曜石在厚度方向相邻（axis=X → 沿 Z 展开，X 为厚度；axis=Z → 沿 X 展开）。
#[must_use]
pub fn detect_portal_frame(
    x: i32,
    z: i32,
    bottom_y: i32,
    axis: PortalAxis,
    block_at: &impl Fn(i32, i32, i32) -> i32,
) -> Option<Vec<(i32, i32, i32)>> {
    let is_obsidian = |bx: i32, by: i32, bz: i32| block_at(bx, by, bz) == block_state_ids::OBSIDIAN;
    let is_air = |bx: i32, by: i32, bz: i32| {
        let state = block_at(bx, by, bz);
        state == 0 || state == block_state_ids::NETHER_PORTAL
    };
    // 主方向（宽 4）：axis=X → 沿 Z 展开（z..z+4），x 厚度（x、x+1）；
    // axis=Z → 沿 X 展开（x..x+4），z 厚度（z、z+1）。
    let (main_a, main_b, thick_a, thick_b) = match axis {
        PortalAxis::X => (z, z + 3, x, x + 1),
        PortalAxis::Z => (x, x + 3, z, z + 1),
    };
    // 两列黑曜石柱：厚度的两个坐标 × 主方向两端；检查底部（bottom）与顶部
    // （bottom+4）各 1 格（vanilla 框架：底部/顶部横梁各 1 格高）
    for thick in [thick_a, thick_b] {
        for main in [main_a, main_b] {
            let (bx, bz) = match axis {
                PortalAxis::X => (thick, main),
                PortalAxis::Z => (main, thick),
            };
            if !is_obsidian(bx, bottom_y, bz) {
                return None;
            }
            if !is_obsidian(bx, bottom_y + 4, bz) {
                return None;
            }
        }
    }
    // 内部 4×3 区域（y=bottom+1 ..= bottom+3）必须是空气或已存在的传送门
    let mut fill = Vec::new();
    for main in main_a..=main_b {
        for thick in [thick_a, thick_b] {
            for by in (bottom_y + 1)..=(bottom_y + 3) {
                let (bx, bz) = match axis {
                    PortalAxis::X => (thick, main),
                    PortalAxis::Z => (main, thick),
                };
                if !is_air(bx, by, bz) {
                    return None;
                }
                fill.push((bx, by, bz));
            }
        }
    }
    Some(fill)
}

/// 传送门方块状态 ID（axis 朝向；默认 axis=x）。
#[must_use]
pub const fn portal_state(axis: PortalAxis) -> i32 {
    match axis {
        // vanilla：nether_portal 状态顺序 axis=x(0) / z(1)；id 5864 是 axis=x
        PortalAxis::X => block_state_ids::NETHER_PORTAL,
        PortalAxis::Z => block_state_ids::NETHER_PORTAL + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{PortalAxis, block_state_ids, detect_portal_frame, portal_state};

    #[test]
    fn detects_complete_frame() {
        // axis=X → 框架沿 Z 展开（z=0..3 宽 4），x 厚度 2 列（x=0/1）。
        // 底部 y=0、顶部 y=4 的黑曜石；内部 y=1..3 空气。
        let mut blocks = std::collections::HashMap::new();
        for x in 0..2 {
            for z in 0..4 {
                blocks.insert((x, 0, z), block_state_ids::OBSIDIAN);
                blocks.insert((x, 4, z), block_state_ids::OBSIDIAN);
            }
        }
        let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
        let fill = detect_portal_frame(0, 0, 0, PortalAxis::X, &get);
        assert!(fill.is_some(), "frame should be detected");
        let fill = fill.unwrap();
        // 内部：z=0..3 × x=0/1 × y=1..3 = 4×2×3 = 24
        assert_eq!(fill.len(), 24);
        for (x, y, z) in &fill {
            assert!((0..2).contains(x));
            assert!((1..=3).contains(y));
            assert!((0..4).contains(z));
        }
    }

    #[test]
    fn rejects_incomplete_frame() {
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, 0, 0), block_state_ids::OBSIDIAN); // 只有一角
        let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
        assert!(detect_portal_frame(0, 0, 0, PortalAxis::X, &get).is_none());
    }

    #[test]
    fn portal_states() {
        assert_eq!(portal_state(PortalAxis::X), block_state_ids::NETHER_PORTAL);
        assert_eq!(portal_state(PortalAxis::Z), block_state_ids::NETHER_PORTAL + 1);
    }
}
