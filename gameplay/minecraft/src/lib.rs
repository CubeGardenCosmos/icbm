//! `minecraft` 命名空间的主要内容。
//!
//! - [`blocks`]：方块注册表（状态 ID ↔ 名 + 行为骨架）
//! - [`mobs`]：生物注册表与生成/AI 行为
//! - [`sounds`]：声音事件最小集
//! - [`gamerules`]：1.21.1 全量 51 条游戏规则
//! - [`commands`]：`/gamemode`、`/gamerule` 命令定义与解析
//! - [`fluid`]：流体模拟（`FlowingFluid` 状态机，计划 tick 驱动）
//! - [`redstone`]：红石线/灯/火把信号计算
//! - [`crops`]：农作物随机 tick 生长
//! - [`weather`]：天气状态机与 `GAME_EVENT` 广播
//! - [`portal`]：地狱门框架检测与构建
//! - [`pistons`]：活塞伸出/缩回与推动
//! - [`entities`]：实体（TNT/掉落物）与爆炸计算
//! - [`mob_behavior`]：友好生物生成与随机游走 AI
//!
//! 玩法逻辑（生物行为、方块、流体等）统一在本 crate 编译，region 只做
//! 模拟编排（tick 循环、IPC、发包、store 存取），不内嵌玩法实现。
//! 模块保持纯函数/闭包/trait 形态（输入世界状态 → 输出更新/动作），
//! 由 region 提供 store 访问与发包。

pub mod blocks;
pub mod commands;
pub mod creeper;
pub mod crops;
pub mod entities;
pub mod fluid;
pub mod gamerules;
pub mod inventory;
pub mod mob_behavior;
pub mod mobs;
pub mod pistons;
pub mod portal;
pub mod redstone;
pub mod sounds;
pub mod weather;
