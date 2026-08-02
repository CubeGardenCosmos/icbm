//! 地形生成器。
//!
//! Phase 1：主世界/地狱/末地统一为 flat 超平坦（对照
//! `FlatChunkGenerator.java` 的层列表模型）。输出为每个区块的方块状态
//! ID 列（`ChunkColumn`），由 region 负责转网络数据（`icbm-mc-767`）与
//! 存档数据（`icbm-anvil`）。

use icbm_mc_767::packets::play::block_state_ids as ids;

/// 维度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    Overworld,
    Nether,
    End,
}

/// 一个方块层。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layer {
    /// 层高（方块数）。
    pub height: u32,
    /// 方块状态 ID（网络全局 ID）。
    pub block_state_id: i32,
}

/// 世界 Y 轴范围（自底向上）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldHeight {
    /// 最低 Y。
    pub min_y: i32,
    /// 总高度（区块数）。
    pub height: i32,
}

impl WorldHeight {
    /// section 数。
    #[must_use]
    pub const fn section_count(&self) -> usize {
        (self.height / 16) as usize
    }

    /// 底部 section Y。
    #[must_use]
    pub const fn bottom_section_y(&self) -> i32 {
        self.min_y / 16
    }
}

/// flat 生成配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatConfig {
    /// 自底向上的层列表（从 y=0 开始铺设）。
    pub layers: Vec<Layer>,
    /// 生物群系网络 ID。
    pub biome_id: i32,
    /// 世界 Y 范围。
    pub world_height: WorldHeight,
}

impl Dimension {
    /// 该维度的 flat 配置。
    #[must_use]
    pub fn flat_config(&self) -> FlatConfig {
        match self {
            // 原版 superflat 默认预设：bedrock / dirt×2 / grass，y 0..3
            Self::Overworld => FlatConfig {
                layers: vec![
                    Layer {
                        height: 1,
                        block_state_id: ids::BEDROCK,
                    },
                    Layer {
                        height: 2,
                        block_state_id: ids::DIRT,
                    },
                    Layer {
                        height: 1,
                        block_state_id: ids::GRASS_BLOCK,
                    },
                ],
                biome_id: biome_ids::PLAINS,
                world_height: WorldHeight {
                    min_y: -64,
                    height: 384,
                },
            },
            // bedrock 地板 + netherrack + bedrock 天花板（y 0..255）
            Self::Nether => FlatConfig {
                layers: vec![
                    Layer {
                        height: 1,
                        block_state_id: ids::BEDROCK,
                    },
                    Layer {
                        height: 126,
                        block_state_id: ids::NETHERRACK,
                    },
                    Layer {
                        height: 1,
                        block_state_id: ids::BEDROCK,
                    },
                ],
                biome_id: biome_ids::NETHER_WASTES,
                world_height: WorldHeight {
                    min_y: 0,
                    height: 256,
                },
            },
            // 末地石一层
            Self::End => FlatConfig {
                layers: vec![Layer {
                    height: 1,
                    block_state_id: ids::END_STONE,
                }],
                biome_id: biome_ids::THE_END,
                world_height: WorldHeight {
                    min_y: 0,
                    height: 256,
                },
            },
        }
    }

    /// 维度名（与协议 `dimension_names` 一致）。
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Overworld => "minecraft:overworld",
            Self::Nether => "minecraft:the_nether",
            Self::End => "minecraft:the_end",
        }
    }
}

/// 生物群系网络 ID（引用于 `icbm-mc-767`）。
mod biome_ids {
    use icbm_mc_767::packets::play::biome_ids;

    pub const PLAINS: i32 = biome_ids::PLAINS;
    pub const NETHER_WASTES: i32 = biome_ids::NETHER_WASTES;
    pub const THE_END: i32 = biome_ids::THE_END;
}

/// 生成的区块列（方块状态 ID）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkColumn {
    /// 区块 X。
    pub x: i32,
    /// 区块 Z。
    pub z: i32,
    /// 底部 section Y。
    pub bottom_section_y: i32,
    /// sections（自底向上，每节 4096 个方块状态 ID，索引 `(y<<4|z)<<4|x`）。
    pub sections: Vec<[i32; 4096]>,
    /// 生物群系网络 ID。
    pub biome_id: i32,
    /// 地表高度（最高非空气方块 y + 1）。
    pub surface_height: u16,
}

impl ChunkColumn {
    /// 世界 Y → section 索引与节内 Y。
    #[must_use]
    pub const fn section_index_of(&self, world_y: i32) -> usize {
        ((world_y - self.bottom_section_y * 16) / 16) as usize
    }

    /// 读取世界坐标方块状态 ID。
    #[must_use]
    pub fn block_at(&self, x: u32, world_y: i32, z: u32) -> i32 {
        let section = self.section_index_of(world_y);
        let local_y = (world_y - self.bottom_section_y * 16) % 16;
        self.sections[section][((local_y << 4 | z as i32) << 4 | x as i32) as usize]
    }

    /// 计算每列高度（最高非空气方块 y + 1），供 heightmap 使用。
    #[must_use]
    pub fn heightmap_heights(&self) -> [u16; 256] {
        let mut heights = [0u16; 256];
        for (column, height) in heights.iter_mut().enumerate() {
            let x = (column & 0xF) as u32;
            let z = (column >> 4) as u32;
            let base_y = self.bottom_section_y * 16;
            let max_y = base_y + (self.sections.len() * 16) as i32;
            let mut top = base_y;
            for y in base_y..max_y {
                if self.block_at(x, y, z) != 0 {
                    top = y;
                }
            }
            *height = u16::try_from(top + 1).unwrap_or(0);
        }
        heights
    }
}

/// 生成 flat 区块。
#[must_use]
pub fn generate_chunk(dimension: Dimension, x: i32, z: i32) -> ChunkColumn {
    let config = dimension.flat_config();
    let section_count = config.world_height.section_count();
    let mut sections = vec![[0i32; 4096]; section_count];

    let mut layer_start_y = 0i32;
    for layer in &config.layers {
        for offset in 0..layer.height {
            let world_y = layer_start_y + offset as i32;
            let section = ((world_y - config.world_height.min_y) / 16) as usize;
            let local_y = (world_y - config.world_height.min_y) % 16;
            let base = ((local_y << 4) << 4) as usize;
            for z_local in 0..16 {
                let row = base + (z_local << 4) as usize;
                sections[section][row..row + 16].fill(layer.block_state_id);
            }
        }
        layer_start_y += layer.height as i32;
    }

    let column = ChunkColumn {
        x,
        z,
        bottom_section_y: config.world_height.bottom_section_y(),
        sections,
        biome_id: config.biome_id,
        surface_height: 0,
    };
    let surface_height = column
        .heightmap_heights()
        .iter()
        .max()
        .copied()
        .unwrap_or(0);
    ChunkColumn {
        surface_height,
        ..column
    }
}

#[cfg(test)]
mod tests {
    use super::{Dimension, generate_chunk};
    use icbm_mc_767::packets::play::block_state_ids as ids;

    #[test]
    fn overworld_flat_structure() {
        let chunk = generate_chunk(Dimension::Overworld, 0, 0);
        assert_eq!(chunk.bottom_section_y, -4);
        assert_eq!(chunk.sections.len(), 24);
        assert_eq!(chunk.block_at(0, 0, 0), ids::BEDROCK);
        assert_eq!(chunk.block_at(0, 1, 0), ids::DIRT);
        assert_eq!(chunk.block_at(0, 2, 0), ids::DIRT);
        assert_eq!(chunk.block_at(0, 3, 0), ids::GRASS_BLOCK);
        assert_eq!(chunk.block_at(0, 4, 0), ids::AIR);
        assert_eq!(chunk.block_at(0, -1, 0), ids::AIR);
        assert_eq!(chunk.surface_height, 4);
        let heights = chunk.heightmap_heights();
        assert!(heights.iter().all(|&h| h == 4));
    }

    #[test]
    fn nether_flat_structure() {
        let chunk = generate_chunk(Dimension::Nether, 1, 2);
        assert_eq!(chunk.bottom_section_y, 0);
        assert_eq!(chunk.sections.len(), 16);
        assert_eq!(chunk.block_at(0, 0, 0), ids::BEDROCK);
        assert_eq!(chunk.block_at(0, 1, 0), ids::NETHERRACK);
        assert_eq!(chunk.block_at(0, 127, 0), ids::BEDROCK);
        assert_eq!(chunk.block_at(0, 128, 0), ids::AIR);
        assert_eq!(chunk.surface_height, 128);
    }

    #[test]
    fn end_flat_structure() {
        let chunk = generate_chunk(Dimension::End, 0, 0);
        assert_eq!(chunk.block_at(0, 0, 0), ids::END_STONE);
        assert_eq!(chunk.block_at(0, 1, 0), ids::AIR);
        assert_eq!(chunk.surface_height, 1);
    }

    #[test]
    fn biome_ids_match_protocol() {
        assert_eq!(Dimension::Overworld.flat_config().biome_id, 1);
        assert_eq!(Dimension::Nether.flat_config().biome_id, 2);
        assert_eq!(Dimension::End.flat_config().biome_id, 3);
    }

    #[test]
    fn negative_region_chunks() {
        let chunk = generate_chunk(Dimension::Overworld, -1, -1);
        assert_eq!(chunk.x, -1);
        assert_eq!(chunk.block_at(15, 3, 15), ids::GRASS_BLOCK);
        assert_eq!(chunk.heightmap_heights()[255], 4);
    }
}
