//! `icbm` 服务器特定命名空间内容。
//!
//! Phase 1 提供 `/bmtool` 命令（仅 `tps` 子命令）。命令数据由 core
//! 聚合各 region 心跳后提供，本模块只负责语法与输出格式化。

pub mod bmtool;
