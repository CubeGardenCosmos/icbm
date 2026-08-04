//! icbm 服务器 core（同一二进制默认模式）。
//!
//! 职责：配置、控制台、网络状态机（握手/状态/登录/配置/Play）、
//! region 编排（子进程拉起/注册/心跳/沉睡/卸载）、MOTD 与 favicon。

pub mod client;
pub mod config;
pub mod console;
pub mod lists;
pub mod net;
pub mod region_registry;
pub mod server;
pub mod world;
