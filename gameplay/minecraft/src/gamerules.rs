//! 游戏规则（对照 1.21.1 `game_rules.json`，共 51 条）。
//!
//! 规则名使用原版下划线命名（如 `doDaylightCycle`）。类型为布尔或整数。

use std::collections::HashMap;

/// 规则值类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleType {
    Bool,
    Int,
}

/// 规则定义。
#[derive(Debug, Clone, Copy)]
pub struct GameRule {
    /// 规则名。
    pub name: &'static str,
    /// 值类型。
    pub rule_type: RuleType,
    /// 默认值。
    pub default: &'static str,
}

/// 1.21.1 全量规则表（40 布尔 + 11 整数）。
pub const GAME_RULES: &[GameRule] = &[
    GameRule {
        name: "announceAdvancements",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "blockExplosionDropDecay",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "commandBlockOutput",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "commandModificationBlockLimit",
        rule_type: RuleType::Int,
        default: "32768",
    },
    GameRule {
        name: "disableElytraMovementCheck",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "disableRaids",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "doDaylightCycle",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doEntityDrops",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doFireTick",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doImmediateRespawn",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "doInsomnia",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doLimitedCrafting",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "doMobLoot",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doMobSpawning",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doPatrolSpawning",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doTileDrops",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doTraderSpawning",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doVinesSpread",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doWardenSpawning",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "doWeatherCycle",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "drowningDamage",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "enderPearlsVanishOnDeath",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "fallDamage",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "fireDamage",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "forgiveDeadPlayers",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "freezeDamage",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "globalSoundEvents",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "keepInventory",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "lavaSourceConversion",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "logAdminCommands",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "maxCommandChainLength",
        rule_type: RuleType::Int,
        default: "65536",
    },
    GameRule {
        name: "maxCommandForkCount",
        rule_type: RuleType::Int,
        default: "65536",
    },
    GameRule {
        name: "maxEntityCramming",
        rule_type: RuleType::Int,
        default: "24",
    },
    GameRule {
        name: "mobExplosionDropDecay",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "mobGriefing",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "naturalRegeneration",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "playersNetherPortalCreativeDelay",
        rule_type: RuleType::Int,
        default: "1",
    },
    GameRule {
        name: "playersNetherPortalDefaultDelay",
        rule_type: RuleType::Int,
        default: "80",
    },
    GameRule {
        name: "playersSleepingPercentage",
        rule_type: RuleType::Int,
        default: "100",
    },
    GameRule {
        name: "projectilesCanBreakBlocks",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "randomTickSpeed",
        rule_type: RuleType::Int,
        default: "3",
    },
    GameRule {
        name: "reducedDebugInfo",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "sendCommandFeedback",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "showDeathMessages",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "snowAccumulationHeight",
        rule_type: RuleType::Int,
        default: "1",
    },
    GameRule {
        name: "spawnChunkRadius",
        rule_type: RuleType::Int,
        default: "2",
    },
    GameRule {
        name: "spawnRadius",
        rule_type: RuleType::Int,
        default: "10",
    },
    GameRule {
        name: "spectatorsGenerateChunks",
        rule_type: RuleType::Bool,
        default: "true",
    },
    GameRule {
        name: "tntExplosionDropDecay",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "universalAnger",
        rule_type: RuleType::Bool,
        default: "false",
    },
    GameRule {
        name: "waterSourceConversion",
        rule_type: RuleType::Bool,
        default: "true",
    },
];

/// 按名查规则定义。
#[must_use]
pub fn find(name: &str) -> Option<&'static GameRule> {
    GAME_RULES.iter().find(|rule| rule.name == name)
}

/// 规则名 → 当前值表。
#[derive(Debug, Clone, Default)]
pub struct GameRuleStore {
    values: HashMap<String, String>,
}

impl GameRuleStore {
    /// 新建（全部默认值）。
    #[must_use]
    pub fn new() -> Self {
        let mut values = HashMap::new();
        for rule in GAME_RULES {
            values.insert(rule.name.to_owned(), rule.default.to_owned());
        }
        Self { values }
    }

    /// 取值。
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// 设置值（类型校验；非法返回 `Err`）。
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let rule = find(name).ok_or_else(|| format!("未知游戏规则: {name}"))?;
        match rule.rule_type {
            RuleType::Bool => {
                if value == "true" || value == "false" {
                    self.values.insert(name.to_owned(), value.to_owned());
                    Ok(())
                } else {
                    Err(format!("规则 {name} 需要 true 或 false"))
                }
            }
            RuleType::Int => {
                if value.parse::<i32>().is_ok() {
                    self.values.insert(name.to_owned(), value.to_owned());
                    Ok(())
                } else {
                    Err(format!("规则 {name} 需要整数"))
                }
            }
        }
    }

    /// 全部规则（名, 值）。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::{GAME_RULES, GameRuleStore, find};

    #[test]
    fn count_matches_1_21_1() {
        assert_eq!(GAME_RULES.len(), 51);
        assert_eq!(find("doDaylightCycle").unwrap().default, "true");
        assert_eq!(find("randomTickSpeed").unwrap().default, "3");
        assert!(find("nonexistent").is_none());
    }

    #[test]
    fn store_validates() {
        let mut store = GameRuleStore::new();
        assert!(store.set("keepInventory", "true").is_ok());
        assert_eq!(store.get("keepInventory"), Some("true"));
        assert!(store.set("keepInventory", "maybe").is_err());
        assert!(store.set("randomTickSpeed", "abc").is_err());
        assert!(store.set("randomTickSpeed", "5").is_ok());
        assert!(store.set("bogusRule", "true").is_err());
    }
}
