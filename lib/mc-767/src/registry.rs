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
    /// TNT（默认态 unstable=false）。
    pub const TNT: &str = "minecraft:tnt";
    /// 橡木原木。
    pub const OAK_LOG: &str = "minecraft:oak_log";
    /// 橡木木板。
    pub const OAK_PLANKS: &str = "minecraft:oak_planks";
    /// 圆石。
    pub const COBBLESTONE: &str = "minecraft:cobblestone";
    /// 下界传送门方块。
    pub const NETHER_PORTAL: &str = "minecraft:nether_portal";
    /// 红石线。
    pub const REDSTONE_WIRE: &str = "minecraft:redstone_wire";
    /// 红石火把。
    pub const REDSTONE_TORCH: &str = "minecraft:redstone_torch";
    /// 红石灯。
    pub const REDSTONE_LAMP: &str = "minecraft:redstone_lamp";
    /// 红石块。
    pub const REDSTONE_BLOCK: &str = "minecraft:redstone_block";
    /// 耕地。
    pub const FARMLAND: &str = "minecraft:farmland";
    /// 小麦。
    pub const WHEAT: &str = "minecraft:wheat";
    /// 胡萝卜。
    pub const CARROTS: &str = "minecraft:carrots";
    /// 马铃薯。
    pub const POTATOES: &str = "minecraft:potatoes";
    /// 甜菜根。
    pub const BEETROOTS: &str = "minecraft:beetroots";
    /// 南瓜茎。
    pub const PUMPKIN_STEM: &str = "minecraft:pumpkin_stem";
    /// 西瓜茎。
    pub const MELON_STEM: &str = "minecraft:melon_stem";
    /// 可可果。
    pub const COCOA: &str = "minecraft:cocoa";
    /// 命令方块。
    pub const COMMAND_BLOCK: &str = "minecraft:command_block";
    /// 屏障。
    pub const BARRIER: &str = "minecraft:barrier";
    /// 亮度方块（vanilla 注册键 `minecraft:light`）。
    pub const LIGHT: &str = "minecraft:light";
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
        ids::TNT => Some(names::TNT),
        ids::OAK_LOG => Some(names::OAK_LOG),
        ids::OAK_PLANKS => Some(names::OAK_PLANKS),
        ids::COBBLESTONE => Some(names::COBBLESTONE),
        ids::NETHER_PORTAL => Some(names::NETHER_PORTAL),
        ids::REDSTONE_WIRE => Some(names::REDSTONE_WIRE),
        ids::REDSTONE_TORCH_OFF => Some(names::REDSTONE_TORCH),
        ids::REDSTONE_LAMP => Some(names::REDSTONE_LAMP),
        ids::REDSTONE_BLOCK => Some(names::REDSTONE_BLOCK),
        ids::FARMLAND => Some(names::FARMLAND),
        ids::WHEAT_AGE_0 => Some(names::WHEAT),
        ids::CARROTS_AGE_0 => Some(names::CARROTS),
        ids::POTATOES_AGE_0 => Some(names::POTATOES),
        ids::BEETROOTS_AGE_0 => Some(names::BEETROOTS),
        ids::PUMPKIN_STEM_AGE_0 => Some(names::PUMPKIN_STEM),
        ids::MELON_STEM_AGE_0 => Some(names::MELON_STEM),
        ids::COCOA_AGE_0 => Some(names::COCOA),
        ids::COMMAND_BLOCK => Some(names::COMMAND_BLOCK),
        ids::BARRIER => Some(names::BARRIER),
        ids::LIGHT_BLOCK => Some(names::LIGHT),
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
        names::TNT => Some(ids::TNT),
        names::OAK_LOG => Some(ids::OAK_LOG),
        names::OAK_PLANKS => Some(ids::OAK_PLANKS),
        names::COBBLESTONE => Some(ids::COBBLESTONE),
        names::NETHER_PORTAL => Some(ids::NETHER_PORTAL),
        names::REDSTONE_WIRE => Some(ids::REDSTONE_WIRE),
        names::REDSTONE_TORCH => Some(ids::REDSTONE_TORCH_OFF),
        names::REDSTONE_LAMP => Some(ids::REDSTONE_LAMP),
        names::REDSTONE_BLOCK => Some(ids::REDSTONE_BLOCK),
        names::FARMLAND => Some(ids::FARMLAND),
        names::WHEAT => Some(ids::WHEAT_AGE_0),
        names::CARROTS => Some(ids::CARROTS_AGE_0),
        names::POTATOES => Some(ids::POTATOES_AGE_0),
        names::BEETROOTS => Some(ids::BEETROOTS_AGE_0),
        names::PUMPKIN_STEM => Some(ids::PUMPKIN_STEM_AGE_0),
        names::MELON_STEM => Some(ids::MELON_STEM_AGE_0),
        names::COCOA => Some(ids::COCOA_AGE_0),
        names::COMMAND_BLOCK => Some(ids::COMMAND_BLOCK),
        names::BARRIER => Some(ids::BARRIER),
        names::LIGHT => Some(ids::LIGHT_BLOCK),
        _ => None,
    }
}

/// 粒子类型名（不含 `minecraft:` 前缀）→ 网络注册表 ID。
///
/// 数据来自 vanilla 1.21.1 `ParticleTypes` 注册序（`ParticleTypes.PACKET_CODEC`
/// 即 `Registries.PARTICLE_TYPE` 的注册序）。客户端内置全部粒子；本表只列
/// 服务端可能发送的简单粒子（`SimpleParticleType`，无附加数据）。
#[must_use]
pub fn particle_id(name: &str) -> Option<i32> {
    match name {
        "explosion" => Some(22),
        "explosion_emitter" => Some(21),
        "large_smoke" => Some(49),
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
