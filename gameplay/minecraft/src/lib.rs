//! `minecraft` 命名空间的主要内容。
//!
//! Phase 1 提供注册表骨架与命令定义：
//! - [`blocks`]：方块注册表（状态 ID ↔ 名 + 行为骨架）
//! - [`mobs`]：生物注册表骨架
//! - [`sounds`]：声音事件最小集
//! - [`gamerules`]：1.21.1 全量 51 条游戏规则
//! - [`commands`]：`/gamemode`、`/gamerule` 命令定义与解析

pub mod blocks;
pub mod commands;
pub mod gamerules;
pub mod mobs;
pub mod sounds;
