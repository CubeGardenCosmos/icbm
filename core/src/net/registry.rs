//! 配置阶段的注册表数据（1.21.1 最小集）。
//!
//! - `minecraft:dimension_type`：4 条目（`overworld` / `overworld_caves` /
//!   `the_end` / `the_nether`），网络 ID 见 `dimension_type_ids`
//! - `minecraft:worldgen/biome`：4 条目（`the_void` / `plains` /
//!   `nether_wastes` / `the_end`），顺序即 biome 网络 ID（见 `biome_ids`）

use icbm_mc_767::nbt::{Nbt, NbtCompound};
use icbm_mc_767::packets::config::{
    CRegistryData, CUpdateEnabledFeatures, CUpdateTags, RegistryEntry,
};

fn int_list(min: i32, max: i32) -> Nbt {
    let mut value = NbtCompound::new();
    value.insert("min_inclusive", Nbt::Int(min));
    value.insert("max_inclusive", Nbt::Int(max));
    let mut uniform = NbtCompound::new();
    uniform.insert("type", Nbt::String("minecraft:uniform".to_owned()));
    uniform.insert("value", Nbt::Compound(Box::new(value)));
    Nbt::Compound(Box::new(uniform))
}

/// 维度类型字段（对照 `DimensionType.java` CODEC）。
#[derive(Clone)]
struct DimensionTypeSpec {
    has_skylight: bool,
    has_ceiling: bool,
    ultrawarm: bool,
    natural: bool,
    coordinate_scale: f32,
    bed_works: bool,
    respawn_anchor_works: bool,
    min_y: i32,
    height: i32,
    logical_height: i32,
    infiniburn: &'static str,
    effects: &'static str,
    ambient_light: f32,
    piglin_safe: bool,
    has_raids: bool,
    monster_spawn_light: Nbt,
    monster_spawn_block_light_limit: i32,
}

/// 构造维度类型 NBT（1.21.1 codec 字段）。
fn dimension_type_nbt(spec: DimensionTypeSpec) -> Nbt {
    let mut data = NbtCompound::new();
    data.insert("has_skylight", Nbt::Byte(i8::from(spec.has_skylight)));
    data.insert("has_ceiling", Nbt::Byte(i8::from(spec.has_ceiling)));
    data.insert("ultrawarm", Nbt::Byte(i8::from(spec.ultrawarm)));
    data.insert("natural", Nbt::Byte(i8::from(spec.natural)));
    data.insert(
        "coordinate_scale",
        Nbt::Double(f64::from(spec.coordinate_scale)),
    );
    data.insert("bed_works", Nbt::Byte(i8::from(spec.bed_works)));
    data.insert(
        "respawn_anchor_works",
        Nbt::Byte(i8::from(spec.respawn_anchor_works)),
    );
    data.insert("min_y", Nbt::Int(spec.min_y));
    data.insert("height", Nbt::Int(spec.height));
    data.insert("logical_height", Nbt::Int(spec.logical_height));
    data.insert("infiniburn", Nbt::String(spec.infiniburn.to_owned()));
    data.insert("effects", Nbt::String(spec.effects.to_owned()));
    data.insert("ambient_light", Nbt::Float(spec.ambient_light));
    data.insert("piglin_safe", Nbt::Byte(i8::from(spec.piglin_safe)));
    data.insert("has_raids", Nbt::Byte(i8::from(spec.has_raids)));
    data.insert("monster_spawn_light_level", spec.monster_spawn_light);
    data.insert(
        "monster_spawn_block_light_limit",
        Nbt::Int(spec.monster_spawn_block_light_limit),
    );
    Nbt::Compound(Box::new(data))
}

/// `minecraft:dimension_type` 注册表（顺序即网络 ID，对照 `DimensionTypes.java`）。
#[must_use]
pub fn dimension_type_registry() -> CRegistryData {
    let overworld_spec = DimensionTypeSpec {
        has_skylight: true,
        has_ceiling: false,
        ultrawarm: false,
        natural: true,
        coordinate_scale: 1.0,
        bed_works: true,
        respawn_anchor_works: true,
        min_y: -64,
        height: 384,
        logical_height: 384,
        infiniburn: "#minecraft:infiniburn_overworld",
        effects: "minecraft:overworld",
        ambient_light: 0.0,
        piglin_safe: false,
        has_raids: true,
        monster_spawn_light: int_list(0, 7),
        monster_spawn_block_light_limit: 0,
    };
    let overworld = dimension_type_nbt(overworld_spec.clone());
    let overworld_caves = dimension_type_nbt(DimensionTypeSpec {
        effects: "minecraft:overworld",
        ..overworld_spec.clone()
    });
    let the_nether = dimension_type_nbt(DimensionTypeSpec {
        has_skylight: false,
        has_ceiling: true,
        ultrawarm: true,
        natural: false,
        coordinate_scale: 8.0,
        bed_works: false,
        respawn_anchor_works: false,
        min_y: 0,
        height: 256,
        logical_height: 128,
        infiniburn: "#minecraft:infiniburn_nether",
        effects: "minecraft:the_nether",
        ambient_light: 0.1,
        piglin_safe: true,
        has_raids: false,
        monster_spawn_light: Nbt::Int(7),
        monster_spawn_block_light_limit: 15,
    });
    let the_end = dimension_type_nbt(DimensionTypeSpec {
        has_skylight: false,
        has_ceiling: false,
        ultrawarm: false,
        natural: false,
        bed_works: false,
        respawn_anchor_works: true,
        min_y: 0,
        height: 256,
        logical_height: 256,
        infiniburn: "#minecraft:infiniburn_end",
        effects: "minecraft:the_end",
        ambient_light: 0.0,
        piglin_safe: false,
        has_raids: true,
        monster_spawn_light: Nbt::Int(0),
        monster_spawn_block_light_limit: 0,
        ..overworld_spec
    });

    CRegistryData {
        registry_id: "minecraft:dimension_type".to_owned(),
        entries: vec![
            RegistryEntry {
                id: "minecraft:overworld".to_owned(),
                data: overworld,
            },
            RegistryEntry {
                id: "minecraft:overworld_caves".to_owned(),
                data: overworld_caves,
            },
            RegistryEntry {
                id: "minecraft:the_end".to_owned(),
                data: the_end,
            },
            RegistryEntry {
                id: "minecraft:the_nether".to_owned(),
                data: the_nether,
            },
        ],
    }
}

/// 构造生物群系 NBT（1.21.1 codec：precipitation/temperature/downfall/effects）。
fn biome_nbt(
    has_precipitation: bool,
    temperature: f32,
    downfall: f32,
    sky: i32,
    fog: i32,
    water: i32,
    water_fog: i32,
) -> Nbt {
    let mut effects = NbtCompound::new();
    effects.insert("sky_color", Nbt::Int(sky));
    effects.insert("fog_color", Nbt::Int(fog));
    effects.insert("water_color", Nbt::Int(water));
    effects.insert("water_fog_color", Nbt::Int(water_fog));

    let mut data = NbtCompound::new();
    data.insert("has_precipitation", Nbt::Byte(i8::from(has_precipitation)));
    data.insert("temperature", Nbt::Float(temperature));
    data.insert("downfall", Nbt::Float(downfall));
    data.insert("effects", Nbt::Compound(Box::new(effects)));
    // generation_settings 的必需字段（平铺在顶层；空 carvers/features）
    data.insert("carvers", Nbt::Compound(Box::new(NbtCompound::new())));
    data.insert(
        "features",
        Nbt::List(Box::new(icbm_mc_767::nbt::NbtList::empty())),
    );
    // spawn_settings 的必需字段（平铺在顶层；空 spawners/spawn_costs）
    data.insert("spawners", Nbt::Compound(Box::new(NbtCompound::new())));
    data.insert("spawn_costs", Nbt::Compound(Box::new(NbtCompound::new())));
    Nbt::Compound(Box::new(data))
}

/// `minecraft:worldgen/biome` 注册表（顺序即 biome 网络 ID：0=虚空，
/// 1=平原，2=地狱荒原，3=末地）。
#[must_use]
pub fn biome_registry() -> CRegistryData {
    CRegistryData {
        registry_id: "minecraft:worldgen/biome".to_owned(),
        entries: vec![
            RegistryEntry {
                id: "minecraft:the_void".to_owned(),
                data: biome_nbt(false, 0.5, 0.5, 0x7B7B7B, 0xC0D8FF, 0x3F76E4, 0x050533),
            },
            RegistryEntry {
                id: "minecraft:plains".to_owned(),
                data: biome_nbt(true, 0.8, 0.4, 0x78A7FF, 0xC0D8FF, 0x3F76E4, 0x050533),
            },
            RegistryEntry {
                id: "minecraft:nether_wastes".to_owned(),
                data: biome_nbt(false, 2.0, 0.0, 0x905957, 0x423F42, 0x905957, 0x423F42),
            },
            RegistryEntry {
                id: "minecraft:the_end".to_owned(),
                data: biome_nbt(false, 0.5, 0.5, 0x0, 0xA080A0, 0x0, 0x0),
            },
        ],
    }
}

/// 启用的特性标志（`minecraft:vanilla`）。
#[must_use]
pub fn feature_flags() -> CUpdateEnabledFeatures {
    CUpdateEnabledFeatures {
        features: vec!["minecraft:vanilla".to_owned()],
    }
}

/// 最小标签集（Phase 1 为空，客户端可容忍）。
#[must_use]
pub fn update_tags() -> CUpdateTags {
    CUpdateTags::default()
}

/// 构造已知包列表（1.20.5+ 配置阶段已知资源包协商）。
#[must_use]
pub const fn known_packs() -> Vec<icbm_mc_767::packets::config::KnownPack> {
    // 服务器没有资源包：空列表 → 客户端回空 → 注册表全量 NBT 序列化。
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{biome_registry, dimension_type_registry, feature_flags};
    use icbm_mc_767::nbt::Nbt;

    #[test]
    fn dimension_registry_order() {
        let registry = dimension_type_registry();
        assert_eq!(registry.registry_id, "minecraft:dimension_type");
        assert_eq!(registry.entries.len(), 4);
        assert_eq!(registry.entries[0].id, "minecraft:overworld");
        assert_eq!(registry.entries[3].id, "minecraft:the_nether");
        let data = &registry.entries[0].data;
        let Nbt::Compound(compound) = data else {
            panic!("expected compound");
        };
        assert_eq!(compound.get_bool("natural"), Some(true));
        assert_eq!(compound.get_i32("min_y"), Some(-64));
        assert_eq!(compound.get_string("effects"), Some("minecraft:overworld"));
    }

    #[test]
    fn biome_registry_order() {
        let registry = biome_registry();
        assert_eq!(registry.entries.len(), 4);
        assert_eq!(registry.entries[0].id, "minecraft:the_void");
        assert_eq!(registry.entries[1].id, "minecraft:plains");
        assert_eq!(registry.entries[2].id, "minecraft:nether_wastes");
        assert_eq!(registry.entries[3].id, "minecraft:the_end");
    }

    #[test]
    fn features_contains_vanilla() {
        assert_eq!(feature_flags().features, vec!["minecraft:vanilla"]);
    }
}
