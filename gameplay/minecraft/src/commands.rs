//! `/gamemode` 与 `/gamerule` 命令定义与解析。
//!
//! 命令执行发生在 core（需要玩家/世界状态）；本模块只负责语法定义、
//! 解析与校验，保证 gameplay 与 core 解耦。
//!
//! 游戏模式（对应协议 `game_mode` 字节）。

use crate::gamerules::GameRuleStore;

/// 游戏模式（对应协议 `game_mode` 字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GameMode {
    Survival = 0,
    Creative = 1,
    Adventure = 2,
    Spectator = 3,
}

impl GameMode {
    /// 从协议字节解析。
    #[must_use]
    pub const fn from_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Survival),
            1 => Some(Self::Creative),
            2 => Some(Self::Adventure),
            3 => Some(Self::Spectator),
            _ => None,
        }
    }

    /// 协议字节。
    #[must_use]
    pub const fn id(self) -> u8 {
        self as u8
    }

    /// 命令名（`survival` 等）。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Survival => "survival",
            Self::Creative => "creative",
            Self::Adventure => "adventure",
            Self::Spectator => "spectator",
        }
    }

    /// 解析名称或数字（`survival`/`s`/`0`）。
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "survival" | "s" | "0" => Some(Self::Survival),
            "creative" | "c" | "1" => Some(Self::Creative),
            "adventure" | "a" | "2" => Some(Self::Adventure),
            "spectator" | "sp" | "3" => Some(Self::Spectator),
            _ => None,
        }
    }
}

/// 解析后的命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/gamemode <模式> [玩家]`。
    GameMode {
        mode: GameMode,
        target: Option<String>,
    },
    /// `/gamerule <规则> [值]`（无值时查询）。
    GameRule { rule: String, value: Option<String> },
    /// `/help`。
    Help,
}

/// 解析命令行的第一段（`/gamemode ...` / `/gamerule ...` / `/help`）。
///
/// 非本命名空间命令返回 `None`（由 icbm 命名空间或 core 处理）。
pub fn parse_command(line: &str) -> Option<Command> {
    let line = line.strip_prefix('/')?.trim();
    let mut parts = line.split_whitespace();
    let name = parts.next()?;
    match name {
        "gamemode" | "gm" => {
            let mode = GameMode::parse(parts.next()?)?;
            Some(Command::GameMode {
                mode,
                target: parts.next().map(str::to_owned),
            })
        }
        "gamerule" => {
            let rule = parts.next()?.to_owned();
            Some(Command::GameRule {
                rule,
                value: parts.next().map(str::to_owned),
            })
        }
        "help" => Some(Command::Help),
        _ => None,
    }
}

/// 执行 `/gamerule`（查询或设置），返回反馈文本。
pub fn apply_gamerule(
    store: &mut GameRuleStore,
    rule: &str,
    value: Option<&str>,
) -> Result<String, String> {
    match value {
        Some(value) => {
            store.set(rule, value)?;
            Ok(format!("游戏规则 {rule} 已设置为 {value}"))
        }
        None => store.get(rule).map_or_else(
            || Err(format!("未知游戏规则: {rule}")),
            |current| Ok(format!("游戏规则 {rule} = {current}")),
        ),
    }
}

/// `/gamemode` 执行结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameModeResult {
    /// 已应用。
    Applied,
    /// 找不到目标玩家。
    PlayerNotFound(String),
}

#[cfg(test)]
mod tests {
    use super::{Command, GameMode, parse_command};

    #[test]
    fn parse_gamemode() {
        assert_eq!(
            parse_command("/gamemode creative"),
            Some(Command::GameMode {
                mode: GameMode::Creative,
                target: None
            })
        );
        assert_eq!(
            parse_command("/gm survival steve"),
            Some(Command::GameMode {
                mode: GameMode::Survival,
                target: Some("steve".to_owned()),
            })
        );
        assert_eq!(
            parse_command("/gamemode 2"),
            Some(Command::GameMode {
                mode: GameMode::Adventure,
                target: None
            })
        );
        assert_eq!(parse_command("/gamemode"), None);
        assert_eq!(parse_command("/gamemode bogus"), None);
    }

    #[test]
    fn parse_gamerule_and_help() {
        assert_eq!(
            parse_command("/gamerule keepInventory true"),
            Some(Command::GameRule {
                rule: "keepInventory".to_owned(),
                value: Some("true".to_owned()),
            })
        );
        assert_eq!(
            parse_command("/gamerule doDaylightCycle"),
            Some(Command::GameRule {
                rule: "doDaylightCycle".to_owned(),
                value: None
            })
        );
        assert_eq!(parse_command("/help"), Some(Command::Help));
        assert_eq!(parse_command("/stop"), None); // 由 core 处理
    }

    #[test]
    fn gamemode_parse_variants() {
        for (input, expected) in [
            ("survival", GameMode::Survival),
            ("s", GameMode::Survival),
            ("0", GameMode::Survival),
            ("creative", GameMode::Creative),
            ("spectator", GameMode::Spectator),
        ] {
            assert_eq!(GameMode::parse(input), Some(expected), "{input}");
        }
        assert_eq!(GameMode::parse("hardcore"), None);
    }
}
