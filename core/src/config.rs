//! 服务器配置。

use std::path::PathBuf;

/// 服务器配置（Phase 1 全部来自命令行/默认值，文件配置后续阶段加入）。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// 监听地址。
    pub address: String,
    /// 世界目录。
    pub world_dir: PathBuf,
    /// datarouter 共享内存名。
    pub shm_name: String,
    /// MOTD。
    pub motd: String,
    /// 最大玩家数。
    pub max_players: i32,
    /// 可视距离（区块）。
    pub view_distance: i32,
    /// 模拟距离（区块）。
    pub simulation_distance: i32,
    /// 压缩阈值（0 表示禁用）。
    pub compression_threshold: i32,
    /// 难度（0=和平…3=困难）。
    pub difficulty: u8,
    /// 出生游戏模式。
    pub default_gamemode: u8,
    /// 心跳超时（秒，region 失联判定）。
    pub region_timeout_secs: u64,
    /// 沉睡后卸载延迟（秒）。
    pub sleep_unload_secs: u64,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:25565".to_owned(),
            world_dir: PathBuf::from("world"),
            shm_name: "/datarouter".to_owned(),
            motd: "ICBM by CubeGarden Studio, READY".to_owned(),
            max_players: 182,
            view_distance: 8,
            simulation_distance: 8,
            compression_threshold: 256,
            difficulty: 2,
            default_gamemode: 0,
            region_timeout_secs: 5,
            sleep_unload_secs: 30,
        }
    }
}
