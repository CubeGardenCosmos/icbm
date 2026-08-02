//! 1.21.1 附魔注册表（网络 ID = vanilla `minecraft:enchantment` 注册序）。
//!
//! 数据来自 vanilla 1.21.1 enchantment 注册表（对照 master `data/src/generated/enchantment.rs`，
//! 即 `net.minecraft.registry.Registries.ENCHANTMENT` 的注册序）。客户端已内置全部附魔，
//! 服务端无需下发；此表用于把附魔名解析为网络 ID（enchantments 数据组件的 `VarInt`）。

/// 附魔名（不含 `minecraft:` 前缀）→ 网络 ID。
#[must_use]
#[allow(clippy::too_many_lines)] // 生成表（vanilla 注册序）
pub fn enchantment_id(name: &str) -> Option<i32> {
    match name {
        "aqua_affinity" => Some(0),
        "bane_of_arthropods" => Some(1),
        "binding_curse" => Some(2),
        "blast_protection" => Some(3),
        "breach" => Some(4),
        "channeling" => Some(5),
        "density" => Some(6),
        "depth_strider" => Some(7),
        "efficiency" => Some(8),
        "feather_falling" => Some(9),
        "fire_aspect" => Some(10),
        "fire_protection" => Some(11),
        "flame" => Some(12),
        "fortune" => Some(13),
        "frost_walker" => Some(14),
        "impaling" => Some(15),
        "infinity" => Some(16),
        "knockback" => Some(17),
        "looting" => Some(18),
        "loyalty" => Some(19),
        "luck_of_the_sea" => Some(20),
        "lure" => Some(21),
        "mending" => Some(22),
        "multishot" => Some(23),
        "piercing" => Some(24),
        "power" => Some(25),
        "projectile_protection" => Some(26),
        "protection" => Some(27),
        "punch" => Some(28),
        "quick_charge" => Some(29),
        "respiration" => Some(30),
        "riptide" => Some(31),
        "sharpness" => Some(32),
        "silk_touch" => Some(33),
        "smite" => Some(34),
        "soul_speed" => Some(35),
        "sweeping_edge" => Some(36),
        "swift_sneak" => Some(37),
        "thorns" => Some(38),
        "unbreaking" => Some(39),
        "vanishing_curse" => Some(40),
        "wind_burst" => Some(41),
        _ => None,
    }
}


/// 网络 ID → 名字（`enchantment_id` 的反向映射）。
#[must_use]
#[allow(clippy::too_many_lines)] // 生成表
pub const fn enchantment_name(id: i32) -> Option<&'static str> {
    match id {
        0 => Some("aqua_affinity"),
        1 => Some("bane_of_arthropods"),
        2 => Some("binding_curse"),
        3 => Some("blast_protection"),
        4 => Some("breach"),
        5 => Some("channeling"),
        6 => Some("density"),
        7 => Some("depth_strider"),
        8 => Some("efficiency"),
        9 => Some("feather_falling"),
        10 => Some("fire_aspect"),
        11 => Some("fire_protection"),
        12 => Some("flame"),
        13 => Some("fortune"),
        14 => Some("frost_walker"),
        15 => Some("impaling"),
        16 => Some("infinity"),
        17 => Some("knockback"),
        18 => Some("looting"),
        19 => Some("loyalty"),
        20 => Some("luck_of_the_sea"),
        21 => Some("lure"),
        22 => Some("mending"),
        23 => Some("multishot"),
        24 => Some("piercing"),
        25 => Some("power"),
        26 => Some("projectile_protection"),
        27 => Some("protection"),
        28 => Some("punch"),
        29 => Some("quick_charge"),
        30 => Some("respiration"),
        31 => Some("riptide"),
        32 => Some("sharpness"),
        33 => Some("silk_touch"),
        34 => Some("smite"),
        35 => Some("soul_speed"),
        36 => Some("sweeping_edge"),
        37 => Some("swift_sneak"),
        38 => Some("thorns"),
        39 => Some("unbreaking"),
        40 => Some("vanishing_curse"),
        41 => Some("wind_burst"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::enchantment_id;

    #[test]
    fn vanilla_enchantment_ids() {
        assert_eq!(enchantment_id("sharpness"), Some(32));
        assert_eq!(enchantment_id("protection"), Some(27));
        assert_eq!(enchantment_id("unbreaking"), Some(39));
    }
}
