//! 方块注册表（Phase 1 最小集）。
//!
//! 状态 ID 与存储名的映射来自 `icbm-mc-767::registry`；此处补充行为属性
//! （硬度）与查询接口，供 region 模拟与后续玩法扩展使用。

use icbm_mc_767::packets::play::block_state_ids as ids;
use icbm_mc_767::registry;

/// 方块条目。
#[derive(Debug, Clone, Copy)]
pub struct BlockEntry {
    /// 存储名（如 `minecraft:stone`）。
    pub name: &'static str,
    /// 默认状态网络 ID。
    pub default_state_id: i32,
    /// 硬度（原版值，用于挖掘时间；Phase 1 仅记录）。
    pub hardness: f32,
}

/// Phase 1 注册方块。
pub const BLOCKS: &[BlockEntry] = &[
    BlockEntry {
        name: registry::names::AIR,
        default_state_id: ids::AIR,
        hardness: 0.0,
    },
    BlockEntry {
        name: registry::names::STONE,
        default_state_id: ids::STONE,
        hardness: 1.5,
    },
    BlockEntry {
        name: registry::names::GRASS_BLOCK,
        default_state_id: ids::GRASS_BLOCK,
        hardness: 0.6,
    },
    BlockEntry {
        name: registry::names::DIRT,
        default_state_id: ids::DIRT,
        hardness: 0.5,
    },
    BlockEntry {
        name: registry::names::BEDROCK,
        default_state_id: ids::BEDROCK,
        hardness: -1.0,
    },
    BlockEntry {
        name: registry::names::WATER,
        default_state_id: ids::WATER,
        hardness: 100.0,
    },
    BlockEntry {
        name: registry::names::LAVA,
        default_state_id: ids::LAVA,
        hardness: 100.0,
    },
    BlockEntry {
        name: registry::names::SAND,
        default_state_id: ids::SAND,
        hardness: 0.5,
    },
    BlockEntry {
        name: registry::names::OBSIDIAN,
        default_state_id: ids::OBSIDIAN,
        hardness: 50.0,
    },
    BlockEntry {
        name: registry::names::NETHERRACK,
        default_state_id: ids::NETHERRACK,
        hardness: 0.4,
    },
    BlockEntry {
        name: registry::names::END_STONE,
        default_state_id: ids::END_STONE,
        hardness: 3.0,
    },
];

/// 按存储名查方块。
#[must_use]
pub fn by_name(name: &str) -> Option<&'static BlockEntry> {
    BLOCKS.iter().find(|entry| entry.name == name)
}

/// 按默认状态 ID 查方块。
#[must_use]
pub fn by_state_id(state_id: i32) -> Option<&'static BlockEntry> {
    BLOCKS
        .iter()
        .find(|entry| entry.default_state_id == state_id)
}

/// 方块是否为空气。
#[must_use]
pub const fn is_air(state_id: i32) -> bool {
    state_id == ids::AIR
}

/// 方块是否不可破坏（硬度 < 0）。
#[must_use]
pub fn is_unbreakable(state_id: i32) -> bool {
    by_state_id(state_id).is_some_and(|entry| entry.hardness < 0.0)
}

#[cfg(test)]
mod tests {
    use super::{BLOCKS, by_name, by_state_id, is_air};
    use icbm_mc_767::packets::play::block_state_ids as ids;

    #[test]
    fn registry_is_complete() {
        for entry in BLOCKS {
            assert_eq!(
                icbm_mc_767::registry::name_to_state_id(entry.name),
                Some(entry.default_state_id),
                "{}",
                entry.name
            );
            assert_eq!(
                icbm_mc_767::registry::state_id_to_name(entry.default_state_id),
                Some(entry.name)
            );
        }
    }

    #[test]
    fn lookup_works() {
        assert_eq!(by_name("minecraft:grass_block").unwrap().hardness, 0.6);
        assert_eq!(by_state_id(ids::BEDROCK).unwrap().name, "minecraft:bedrock");
        assert!(by_name("minecraft:diamond_block").is_none());
        assert!(is_air(ids::AIR));
        assert!(!is_air(ids::STONE));
    }
}
