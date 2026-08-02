//! 方块状态注册表（Phase 1 最小集）。
//!
//! 网络侧使用固定全局状态 ID（客户端内置，见 [`super::play::block_state_ids`]）；
//! 存档侧（Anvil palette）使用方块名。本模块提供 ID ↔ 名称的双向映射，
//! 供 region/terrain-gen/anvil 共用。行为属性（硬度、随机 tick 等）属于
//! gameplay/minecraft，后续阶段在其上扩展。

/// 方块默认状态名（存储 palette 使用）。
pub mod names {
    /// 空气。
    pub const AIR: &str = "minecraft:air";
    /// 石头。
    pub const STONE: &str = "minecraft:stone";
    /// 草方块（默认 snowy=false）。
    pub const GRASS_BLOCK: &str = "minecraft:grass_block";
    /// 泥土。
    pub const DIRT: &str = "minecraft:dirt";
    /// 基岩。
    pub const BEDROCK: &str = "minecraft:bedrock";
    /// 水（默认 level=0）。
    pub const WATER: &str = "minecraft:water";
    /// 熔岩（默认 level=0）。
    pub const LAVA: &str = "minecraft:lava";
    /// 沙子。
    pub const SAND: &str = "minecraft:sand";
    /// 黑曜石。
    pub const OBSIDIAN: &str = "minecraft:obsidian";
    /// 地狱岩。
    pub const NETHERRACK: &str = "minecraft:netherrack";
    /// 末地石。
    pub const END_STONE: &str = "minecraft:end_stone";
}

use crate::packets::play::block_state_ids as ids;

/// 方块默认状态 ID → 存储名。
#[must_use]
pub const fn state_id_to_name(id: i32) -> Option<&'static str> {
    match id {
        ids::AIR => Some(names::AIR),
        ids::STONE => Some(names::STONE),
        ids::GRASS_BLOCK => Some(names::GRASS_BLOCK),
        ids::DIRT => Some(names::DIRT),
        ids::BEDROCK => Some(names::BEDROCK),
        ids::WATER => Some(names::WATER),
        ids::LAVA => Some(names::LAVA),
        ids::SAND => Some(names::SAND),
        ids::OBSIDIAN => Some(names::OBSIDIAN),
        ids::NETHERRACK => Some(names::NETHERRACK),
        ids::END_STONE => Some(names::END_STONE),
        _ => None,
    }
}

/// 存储名 → 方块默认状态 ID。
#[must_use]
pub fn name_to_state_id(name: &str) -> Option<i32> {
    match name {
        names::AIR => Some(ids::AIR),
        names::STONE => Some(ids::STONE),
        names::GRASS_BLOCK => Some(ids::GRASS_BLOCK),
        names::DIRT => Some(ids::DIRT),
        names::BEDROCK => Some(ids::BEDROCK),
        names::WATER => Some(ids::WATER),
        names::LAVA => Some(ids::LAVA),
        names::SAND => Some(ids::SAND),
        names::OBSIDIAN => Some(ids::OBSIDIAN),
        names::NETHERRACK => Some(ids::NETHERRACK),
        names::END_STONE => Some(ids::END_STONE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{name_to_state_id, state_id_to_name};
    use crate::packets::play::block_state_ids;

    #[test]
    fn id_name_round_trip() {
        for id in [
            block_state_ids::AIR,
            block_state_ids::STONE,
            block_state_ids::GRASS_BLOCK,
            block_state_ids::DIRT,
            block_state_ids::BEDROCK,
            block_state_ids::WATER,
            block_state_ids::LAVA,
            block_state_ids::SAND,
            block_state_ids::OBSIDIAN,
            block_state_ids::NETHERRACK,
            block_state_ids::END_STONE,
        ] {
            let name = state_id_to_name(id).unwrap();
            assert_eq!(name_to_state_id(name), Some(id));
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(state_id_to_name(123456), None);
        assert_eq!(name_to_state_id("minecraft:diamond_block"), None);
    }
}
