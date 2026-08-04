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

impl PortalAxis {
    /// 确定性轴选择（坐标奇偶；与 region `create_portal` 一致，core 据此计算
    /// 玩家落点——固定种子可复现）。
    #[must_use]
    pub const fn from_pos(x: i32, z: i32) -> Self {
        if (x ^ z).rem_euclid(2) == 0 {
            Self::X
        } else {
            Self::Z
        }
    }
}

/// 传送门内部中心（玩家传送落点）：axis=X → (x, y, z+1)；axis=Z → (x+1, y, z)。
/// 中心在内部 2×3 区域内（对照 [`build_portal`] 的填充坐标）。
#[must_use]
pub const fn portal_center(x: i32, y: i32, z: i32, axis: PortalAxis) -> (i32, i32, i32) {
    match axis {
        PortalAxis::X => (x, y, z + 1),
        PortalAxis::Z => (x + 1, y, z),
    }
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
    // 经典 vanilla 4×5 框架（对照 `NetherPortal`）：外层宽 4、高 5，单块厚。
    // 底部/顶部横梁各 4 块，两侧立柱各 5 块（共 14 黑曜石）；内部 2×3 填充。
    let (frame_a, frame_b) = match axis {
        PortalAxis::X => (z, z + 3), // 沿 Z 展开：立柱在 z 两端
        PortalAxis::Z => (x, x + 3), // 沿 X 展开：立柱在 x 两端
    };
    // 底部/顶部横梁（4 块）
    for main in 0..4 {
        let (bx, bz) = match axis {
            PortalAxis::X => (x, z + main),
            PortalAxis::Z => (x + main, z),
        };
        if !is_obsidian(bx, bottom_y, bz) || !is_obsidian(bx, bottom_y + 4, bz) {
            return None;
        }
    }
    // 两侧立柱（各 5 块，y = bottom..=bottom+4；底部/顶部与横梁重叠无害）
    for by in 0..=4 {
        for (bx, bz) in [
            match axis {
                PortalAxis::X => (x, frame_a),
                PortalAxis::Z => (frame_a, z),
            },
            match axis {
                PortalAxis::X => (x, frame_b),
                PortalAxis::Z => (frame_b, z),
            },
        ] {
            if !is_obsidian(bx, bottom_y + by, bz) {
                return None;
            }
        }
    }
    // 内部 2×3（y=bottom+1..=bottom+3）必须是空气或已存在的传送门
    let mut fill = Vec::new();
    for main in 1..=2 {
        for by in (bottom_y + 1)..=(bottom_y + 3) {
            let (bx, bz) = match axis {
                PortalAxis::X => (x, z + main),
                PortalAxis::Z => (x + main, z),
            };
            if !is_air(bx, by, bz) {
                return None;
            }
            fill.push((bx, by, bz));
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

/// 传送门构成：`(框架黑曜石坐标列表, 内部传送门方块坐标列表)`。
pub type PortalBlocks = (Vec<(i32, i32, i32)>, Vec<(i32, i32, i32)>);

/// 生成一扇全新传送门（vanilla `PortalForcer.createPortal` 行为）。
///
/// 以 (x, z) 为左下角黑曜石柱、`bottom_y` 为底部横梁 Y，建经典 4×5 单厚框架
/// （14 黑曜石），内部 2×3 填充传送门方块。返回（框架坐标列表，填充坐标列表）。
/// 玩家落点应取内部中心：axis=X → (x, `bottom_y` + 2, z + 1)；axis=Z →
/// (x + 1, `bottom_y` + 2, z)。
#[must_use]
pub fn build_portal(x: i32, z: i32, bottom_y: i32, axis: PortalAxis) -> PortalBlocks {
    let (a, b) = match axis {
        PortalAxis::X => (z, z + 3),
        PortalAxis::Z => (x, x + 3),
    };
    let mut frame = Vec::with_capacity(14);
    let mut fill = Vec::with_capacity(6);
    // 底部/顶部横梁（各 4 块）
    for main in 0..4 {
        let (bx, bz) = match axis {
            PortalAxis::X => (x, z + main),
            PortalAxis::Z => (x + main, z),
        };
        frame.push((bx, bottom_y, bz));
        frame.push((bx, bottom_y + 4, bz));
    }
    // 两侧立柱（各 5 块，含四角；与横梁重叠无害）
    for by in 0..=4 {
        for (bx, bz) in [
            match axis {
                PortalAxis::X => (x, a),
                PortalAxis::Z => (a, z),
            },
            match axis {
                PortalAxis::X => (x, b),
                PortalAxis::Z => (b, z),
            },
        ] {
            frame.push((bx, bottom_y + by, bz));
        }
    }
    // 内部 2×3
    for main in 1..=2 {
        for by in (bottom_y + 1)..=(bottom_y + 3) {
            let (bx, bz) = match axis {
                PortalAxis::X => (x, z + main),
                PortalAxis::Z => (x + main, z),
            };
            fill.push((bx, by, bz));
        }
    }
    (frame, fill)
}

#[cfg(test)]
mod tests {
    use super::{PortalAxis, block_state_ids, detect_portal_frame, portal_state};

    #[test]
    fn detects_complete_frame() {
        // axis=X → 经典 4×5 框架沿 Z 展开：黑曜石 x=0，立柱在 z=0/3，
        // 底部 y=0、顶部 y=4；内部（x=0, z=1..2, y=1..3）空气。
        let mut blocks = std::collections::HashMap::new();
        for z in 0..4 {
            blocks.insert((0, 0, z), block_state_ids::OBSIDIAN); // 底部横梁
            blocks.insert((0, 4, z), block_state_ids::OBSIDIAN); // 顶部横梁
        }
        for y in 0..=4 {
            blocks.insert((0, y, 0), block_state_ids::OBSIDIAN); // 立柱 z=0
            blocks.insert((0, y, 3), block_state_ids::OBSIDIAN); // 立柱 z=3
        }
        let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
        let fill = detect_portal_frame(0, 0, 0, PortalAxis::X, &get);
        assert!(fill.is_some(), "frame should be detected");
        let fill = fill.unwrap();
        // 内部 2×3：z=1..2 × x=0 × y=1..3 = 6
        assert_eq!(fill.len(), 6);
        for (x, y, z) in &fill {
            assert_eq!(*x, 0);
            assert!((1..=3).contains(y));
            assert!((1..=2).contains(z));
        }
    }

    #[test]
    fn detects_axis_z_frame() {
        // axis=Z → 沿 X 展开：黑曜石 z=0，立柱在 x=0/3
        let mut blocks = std::collections::HashMap::new();
        for x in 0..4 {
            blocks.insert((x, 0, 0), block_state_ids::OBSIDIAN);
            blocks.insert((x, 4, 0), block_state_ids::OBSIDIAN);
        }
        for y in 0..=4 {
            blocks.insert((0, y, 0), block_state_ids::OBSIDIAN);
            blocks.insert((3, y, 0), block_state_ids::OBSIDIAN);
        }
        let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
        let fill = detect_portal_frame(0, 0, 0, PortalAxis::Z, &get);
        assert!(fill.is_some());
        let fill = fill.unwrap();
        assert_eq!(fill.len(), 6);
        for (x, y, z) in &fill {
            assert_eq!(*z, 0);
            assert!((1..=3).contains(y));
            assert!((1..=2).contains(x));
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
        assert_eq!(
            portal_state(PortalAxis::Z),
            block_state_ids::NETHER_PORTAL + 1
        );
    }

    #[test]
    fn build_portal_frame_matches_detection() {
        use super::{build_portal, detect_portal_frame};
        // 用 build_portal 生成的方块构造世界，detect_portal_frame 必须能检测到
        for axis in [PortalAxis::X, PortalAxis::Z] {
            let (frame, fill) = build_portal(0, 0, 0, axis);
            let mut blocks = std::collections::HashMap::new();
            for &(x, y, z) in &frame {
                blocks.insert((x, y, z), block_state_ids::OBSIDIAN);
            }
            for &(x, y, z) in &fill {
                blocks.insert((x, y, z), block_state_ids::NETHER_PORTAL);
            }
            let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
            let detected = detect_portal_frame(0, 0, 0, axis, &get);
            assert!(detected.is_some(), "built {axis:?} frame must be detected");
            let detected = detected.unwrap();
            assert_eq!(detected, fill, "detection fill must match build fill");
        }
    }
}
