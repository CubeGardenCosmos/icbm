//! 流体模拟（水、岩浆）：简化扩散模型，供 region tick 调用。
//!
//! 对照 vanilla `FlowingFluid` 的可观察行为简化：
//! - 源块（level=0）向下方与水平方向流动；`getMaxFlowDistance=4`（源块向
//!   四周扩散的路径长度上限），`getLevelDecreasePerBlock=1`；
//! - 流动态（level>0）优先下落，无路时向四周更低处扩散，level 递增；
//! - level ≥ 8 视为衰减（不继续扩散）；level 15 为最大。
//!
//! 状态 ID 布局（vanilla `Block.BLOCK_STATE_REGISTRY`，对照 master
//! `data/src/generated/block.rs`）：water 80..95（`FluidBlock.LEVEL` 0..15），
//! lava 96..111。`waterlogged` 属性属于各具体方块（如台阶），不在流体模块处理。
//!
//! 本模块只做纯计算（输入当前列快照 → 输出待更新的方块与状态），不持有世界
//! 状态；region 每 tick 对已加载区块调用 [`tick_fluid`]。

use icbm_mc_767::packets::play::block_state_ids;

/// 流体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidKind {
    /// 水。
    Water,
    /// 岩浆。
    Lava,
}

/// 状态 ID 是否为流体（水或岩浆）。
#[must_use]
pub const fn is_fluid(state_id: i32) -> bool {
    FluidKind::from_state_id(state_id).is_some()
}

impl FluidKind {
    /// 状态 ID → 流体类型（非流体返回 `None`）。
    #[must_use]
    pub const fn from_state_id(state_id: i32) -> Option<Self> {
        if state_id >= block_state_ids::WATER && state_id < block_state_ids::WATER + 16 {
            Some(Self::Water)
        } else if state_id >= block_state_ids::LAVA && state_id < block_state_ids::LAVA + 16 {
            Some(Self::Lava)
        } else {
            None
        }
    }

    /// 状态 ID → 流体 level（0 源块 … 15 最大衰减）。
    #[must_use]
    pub const fn level_from_state_id(state_id: i32) -> Option<i32> {
        match Self::from_state_id(state_id) {
            Some(Self::Water) => Some(state_id - block_state_ids::WATER),
            Some(Self::Lava) => Some(state_id - block_state_ids::LAVA),
            None => None,
        }
    }

    /// level → 状态 ID。
    #[must_use]
    pub const fn state_id(self, level: i32) -> i32 {
        let base = match self {
            Self::Water => block_state_ids::WATER,
            Self::Lava => block_state_ids::LAVA,
        };
        let level = if level < 0 { 0 } else if level > 15 { 15 } else { level };
        base + level
    }
}

/// 流体 tick 产生的方块更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidUpdate {
    /// X（世界坐标）。
    pub x: i32,
    /// Y。
    pub y: i32,
    /// Z。
    pub z: i32,
    /// 新状态 ID。
    pub state_id: i32,
}

/// 流体扩散：遍历指定区域内的流体方块，生成更新。
///
/// `block_at(x, y, z)` 返回当前状态 ID（`0` = 空气）；`set` 在每次更新后
/// 应用到世界（顺序即 vanilla 的 tick 顺序：先下落再水平）。
/// 简化模型：每 tick 每个流体方块向下或向相邻更低处流一格。
#[allow(clippy::too_many_arguments)] // 区域边界 + 两个回调，语义清晰
pub fn tick_fluid(
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_x: i32,
    max_y: i32,
    max_z: i32,
    mut block_at: impl FnMut(i32, i32, i32) -> i32,
    mut apply: impl FnMut(FluidUpdate),
) {
    for y in (min_y..=max_y).rev() {
        for x in min_x..=max_x {
            for z in min_z..=max_z {
                let state = block_at(x, y, z);
                let Some(kind) = FluidKind::from_state_id(state) else {
                    continue;
                };
                let Some(level) = FluidKind::level_from_state_id(state) else {
                    continue;
                };
                if level >= 8 {
                    continue; // 衰减完成，不再扩散
                }
                // 1. 下落：下方可替换（空气或可被流体替换的方块）
                let below = block_at(x, y - 1, z);
                if below == 0 {
                    apply(FluidUpdate {
                        x,
                        y: y - 1,
                        z,
                        state_id: kind.state_id(level + 1),
                    });
                    continue;
                }
                // 2. 水平扩散：向四个方向中低于或等于当前高度、可替换者流动
                let directions = [(1, 0), (-1, 0), (0, 1), (0, -1)];
                for (dx, dz) in directions {
                    let nx = x + dx;
                    let nz = z + dz;
                    if nx < min_x || nx > max_x || nz < min_z || nz > max_z {
                        continue;
                    }
                    let neighbor = block_at(nx, y, nz);
                    if neighbor == 0 {
                        apply(FluidUpdate {
                            x: nx,
                            y,
                            z: nz,
                            state_id: kind.state_id(level + 1),
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FluidKind, FluidUpdate, block_state_ids, tick_fluid};

    #[test]
    fn fluid_kind_mapping() {
        assert_eq!(FluidKind::from_state_id(block_state_ids::WATER), Some(FluidKind::Water));
        assert_eq!(FluidKind::from_state_id(block_state_ids::WATER + 7), Some(FluidKind::Water));
        assert_eq!(FluidKind::from_state_id(block_state_ids::LAVA), Some(FluidKind::Lava));
        assert_eq!(FluidKind::from_state_id(block_state_ids::LAVA + 15), Some(FluidKind::Lava));
        assert_eq!(FluidKind::from_state_id(block_state_ids::STONE), None);
        assert_eq!(FluidKind::level_from_state_id(block_state_ids::WATER), Some(0));
        assert_eq!(FluidKind::level_from_state_id(block_state_ids::LAVA + 5), Some(5));
        assert_eq!(FluidKind::Water.state_id(3), block_state_ids::WATER + 3);
        assert_eq!(FluidKind::Lava.state_id(15), block_state_ids::LAVA + 15);
    }

    #[test]
    fn water_flows_down() {
        // 3×1×1 竖井：顶部 y=2 放水源，下方 y=0/1 空气 → 逐格下流
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, 2, 0), block_state_ids::WATER);
        let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
        let mut updates = Vec::new();
        tick_fluid(
            0,
            0,
            0,
            2,
            2,
            0,
            |x, y, z| get(x, y, z),
            |update: FluidUpdate| updates.push(update),
        );
        // 水源应向下扩散：y=1 与四个方向之一
        assert!(updates.iter().any(|u| u.y == 1 && u.x == 0 && u.z == 0));
        // 更新后的 level 应递增
        let down = updates
            .iter()
            .find(|u| u.y == 1 && u.x == 0 && u.z == 0)
            .unwrap();
        assert_eq!(down.state_id, block_state_ids::WATER + 1);
    }

    #[test]
    fn water_spreads_horizontally() {
        // 5×1×5 平面：中心 (2,0,2) 水源、下方 (2,-1,2) 基岩阻挡下落 → 四方向各扩散一级
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((2, 0, 2), block_state_ids::WATER);
        blocks.insert((2, -1, 2), block_state_ids::BEDROCK);
        let get = |x: i32, y: i32, z: i32| *blocks.get(&(x, y, z)).unwrap_or(&0);
        let mut updates = Vec::new();
        tick_fluid(
            0,
            -1,
            0,
            4,
            0,
            4,
            |x, y, z| get(x, y, z),
            |update: FluidUpdate| updates.push(update),
        );
        assert!(updates.len() >= 4, "got {updates:?}");
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            assert!(
                updates.iter().any(|u| u.x == 2 + dx && u.z == 2 + dz),
                "missing dir ({dx},{dz}) in {updates:?}"
            );
        }
    }
}
