//! 服务器配置。

use std::path::PathBuf;

use tracing_subscriber::filter::LevelFilter;

/// 服务器配置（Phase 1 全部来自命令行/默认值，文件配置后续阶段加入）。
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// 日志级别（`--log-level`，默认 INFO；同时传递给 region 子进程）。
    pub log_level: LevelFilter,
    /// 监听地址。
    pub address: String,
    /// 世界目录。
    pub world_dir: PathBuf,
    /// datarouter 共享内存对象名（缺省 `/icbm`，可用 `--shm` 覆盖；
    /// 避免与本机其它 datarouter 实例冲突）。
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
    /// worker 进程数（进程池；`ICBM_WORKERS` 可覆盖，默认 cpu-2 至少 1）。
    pub worker_count: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            log_level: LevelFilter::INFO,
            address: "0.0.0.0:25565".to_owned(),
            world_dir: PathBuf::from("world"),
            shm_name: "/icbm".to_owned(),
            motd: "ICBM by CubeGarden Studio, READY".to_owned(),
            max_players: 183,
            view_distance: 8,
            simulation_distance: 8,
            compression_threshold: 256,
            difficulty: 2,
            default_gamemode: 0,
            region_timeout_secs: 5,
            sleep_unload_secs: 30,
            worker_count: 0, // 0 = 按 cpu-2 自动
        }
    }
}

/// 实际 worker 进程数（`ICBM_WORKERS` 环境变量覆盖；默认 cpu-2 至少 1）。
#[must_use]
pub fn effective_worker_count(config: &ServerConfig) -> usize {
    if config.worker_count > 0 {
        return config.worker_count;
    }
    std::env::var("ICBM_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            let cpu = std::thread::available_parallelism().map_or(1, usize::from);
            cpu.saturating_sub(2).max(1)
        })
}
