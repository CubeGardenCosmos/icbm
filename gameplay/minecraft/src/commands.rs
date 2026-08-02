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

/// 顶层命令名（客户端命令树与 Tab 补全建议用；icbm 命名空间的 `bmtool` 由
/// `icbm-gameplay-icbm` 提供，core 汇总时追加）。
pub const COMMAND_NAMES: &[&str] = &[
    "about", "ban", "fill", "gamemode", "gamerule", "give", "help", "setblock", "tp",
    "whitelist",
];

/// 解析后的命令。
#[derive(Debug, Clone, PartialEq)]
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
    /// `/about`（品牌与构建信息）。
    About,
    /// `/tp <玩家> <x> <y> <z>`（或 `/tp <x> <y> <z>`）。
    Teleport {
        target: Option<String>,
        x: f64,
        y: f64,
        z: f64,
    },
    /// `/setblock <x> <y> <z> <方块>`。
    SetBlock {
        x: i32,
        y: i32,
        z: i32,
        block: String,
    },
    /// `/fill <x1> <y1> <z1> <x2> <y2> <z2> <方块>`。
    Fill {
        from: (i32, i32, i32),
        to: (i32, i32, i32),
        block: String,
    },
    /// `/give <玩家> <物品> [数量]`。
    Give {
        target: String,
        item: String,
        count: i32,
    },
    /// `/ban <玩家> [原因]`。
    Ban {
        target: String,
        reason: Option<String>,
    },
    /// `/whitelist on|off|add <玩家>|remove <玩家>|list`。
    Whitelist {
        action: WhitelistAction,
    },
}

/// 解析命令行的第一段（`/gamemode ...` / `/gamerule ...` / `/help`）。
///
/// 前导 `/` 可选（游戏内聊天发送 `/` 开头；控制台输入不带）；
/// 非本命名空间命令返回 `None`（由 icbm 命名空间或 core 处理）。
pub fn parse_command(line: &str) -> Option<Command> {
    let line = line.strip_prefix('/').unwrap_or(line).trim();
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
        "about" => Some(Command::About),
        "tp" => parse_tp(parts),
        "setblock" => parse_setblock(parts),
        "fill" => parse_fill(parts),
        "give" => parse_give(parts),
        "ban" => {
            let target = parts.next()?.to_owned();
            let reason = parts.next().map(str::to_owned);
            Some(Command::Ban { target, reason })
        }
        "whitelist" => parse_whitelist(parts),
        _ => None,
    }
}

/// 解析 `/tp`：`[玩家] <x> <y> <z>`（相对坐标 `~`/`^` 暂不支持）。
fn parse_tp(parts: std::str::SplitWhitespace<'_>) -> Option<Command> {
    let values: Vec<&str> = parts.collect();
    match values.len() {
        3 => {
            let x = values[0].parse().ok()?;
            let y = values[1].parse().ok()?;
            let z = values[2].parse().ok()?;
            Some(Command::Teleport {
                target: None,
                x,
                y,
                z,
            })
        }
        4 => {
            let x = values[1].parse().ok()?;
            let y = values[2].parse().ok()?;
            let z = values[3].parse().ok()?;
            Some(Command::Teleport {
                target: Some(values[0].to_owned()),
                x,
                y,
                z,
            })
        }
        _ => None,
    }
}

/// 解析 `/setblock <x> <y> <z> <方块>`（方块名可带 `minecraft:` 前缀）。
fn parse_setblock(parts: std::str::SplitWhitespace<'_>) -> Option<Command> {
    let values: Vec<&str> = parts.collect();
    if values.len() != 4 {
        return None;
    }
    Some(Command::SetBlock {
        x: values[0].parse().ok()?,
        y: values[1].parse().ok()?,
        z: values[2].parse().ok()?,
        block: normalize_name(values[3]),
    })
}

/// 解析 `/fill <x1> <y1> <z1> <x2> <y2> <z2> <方块>`。
fn parse_fill(parts: std::str::SplitWhitespace<'_>) -> Option<Command> {
    let values: Vec<&str> = parts.collect();
    if values.len() != 7 {
        return None;
    }
    Some(Command::Fill {
        from: (values[0].parse().ok()?, values[1].parse().ok()?, values[2].parse().ok()?),
        to: (values[3].parse().ok()?, values[4].parse().ok()?, values[5].parse().ok()?),
        block: normalize_name(values[6]),
    })
}

/// 解析 `/give <玩家> <物品> [数量]`。
fn parse_give(parts: std::str::SplitWhitespace<'_>) -> Option<Command> {
    let values: Vec<&str> = parts.collect();
    match values.len() {
        2 => Some(Command::Give {
            target: values[0].to_owned(),
            item: normalize_name(values[1]),
            count: 1,
        }),
        3 => Some(Command::Give {
            target: values[0].to_owned(),
            item: normalize_name(values[1]),
            count: values[2].parse().ok()?,
        }),
        _ => None,
    }
}

/// 白名单动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhitelistAction {
    /// 启用白名单。
    On,
    /// 关闭白名单。
    Off,
    /// 添加玩家。
    Add(String),
    /// 移除玩家。
    Remove(String),
    /// 列出玩家。
    List,
}

/// 解析 `/whitelist` 子命令。
fn parse_whitelist(mut parts: std::str::SplitWhitespace<'_>) -> Option<Command> {
    let action = parts.next()?;
    let action = match action {
        "on" => WhitelistAction::On,
        "off" => WhitelistAction::Off,
        "list" => WhitelistAction::List,
        "add" => WhitelistAction::Add(parts.next()?.to_owned()),
        "remove" => WhitelistAction::Remove(parts.next()?.to_owned()),
        _ => return None,
    };
    Some(Command::Whitelist { action })
}

/// 规范化方块/物品名：去 `minecraft:` 前缀（存储与网络 ID 表使用裸名）。
#[must_use]
pub fn normalize_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}
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
        // 控制台输入无前导 `/`（游戏内聊天带 `/`）：两者等价。
        assert_eq!(
            parse_command("gamemode creative"),
            Some(Command::GameMode {
                mode: GameMode::Creative,
                target: None
            })
        );
        assert_eq!(
            parse_command("gamerule keepInventory true"),
            Some(Command::GameRule {
                rule: "keepInventory".to_owned(),
                value: Some("true".to_owned()),
            })
        );
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
        assert_eq!(parse_command("/about"), Some(Command::About));
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
