//! 生物注册表（骨架）。
//!
//! Phase 1 仅定义注册表结构与少量条目；实体实例化、AI 与属性在后续阶段
//! 实现。

/// 生物类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobType {
    Player,
    Zombie,
    Skeleton,
    Cow,
    Pig,
    Chicken,
    Sheep,
    Villager,
}

impl MobType {
    /// 实体类型注册表名（`minecraft:zombie` 等）。
    #[must_use]
    pub const fn registry_name(self) -> &'static str {
        match self {
            Self::Player => "minecraft:player",
            Self::Zombie => "minecraft:zombie",
            Self::Skeleton => "minecraft:skeleton",
            Self::Cow => "minecraft:cow",
            Self::Pig => "minecraft:pig",
            Self::Chicken => "minecraft:chicken",
            Self::Sheep => "minecraft:sheep",
            Self::Villager => "minecraft:villager",
        }
    }
}

/// 从注册表名解析（未知返回 `None`）。
#[must_use]
pub fn from_registry_name(name: &str) -> Option<MobType> {
    match name {
        "minecraft:player" => Some(MobType::Player),
        "minecraft:zombie" => Some(MobType::Zombie),
        "minecraft:skeleton" => Some(MobType::Skeleton),
        "minecraft:cow" => Some(MobType::Cow),
        "minecraft:pig" => Some(MobType::Pig),
        "minecraft:chicken" => Some(MobType::Chicken),
        "minecraft:sheep" => Some(MobType::Sheep),
        "minecraft:villager" => Some(MobType::Villager),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MobType, from_registry_name};

    #[test]
    fn registry_name_round_trip() {
        for mob in [
            MobType::Player,
            MobType::Zombie,
            MobType::Cow,
            MobType::Villager,
        ] {
            assert_eq!(from_registry_name(mob.registry_name()), Some(mob));
        }
        assert_eq!(from_registry_name("minecraft:ender_dragon"), None);
    }
}
