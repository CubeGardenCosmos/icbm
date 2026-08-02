//! icbm 主二进制入口。
//!
//! 同一二进制两种模式：
//! - 默认：core 模式（服务器主进程）
//! - `--region <rx> <rz>`：region 子进程模式（core 拉起）

use std::path::PathBuf;

use tracing::{debug, error, info, warn};
use tracing_subscriber::filter::LevelFilter;

use icbm_core::console::{init_logging, parse_log_level};
use icbm_region::{RegionConfig, RegionId};

/// 缺省 datarouter 共享内存对象名（可用 `--shm` 覆盖；POSIX shm 名以 `/` 开头）。
const DEFAULT_SHM_NAME: &str = "/icbm";

/// 缺省日志级别。
const DEFAULT_LOG_LEVEL: LevelFilter = LevelFilter::INFO;

fn parse_args() -> Result<Launch, String> {
    let mut args = std::env::args().skip(1);
    let mut world: Option<PathBuf> = None;
    let mut shm: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut region: Option<(i32, i32)> = None;
    let mut log_level = DEFAULT_LOG_LEVEL;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--region" => {
                let rx = args
                    .next()
                    .ok_or_else(|| "--region 需要 <rx> <rz>".to_owned())?
                    .parse::<i32>()
                    .map_err(|e| format!("rx 非法: {e}"))?;
                let rz = args
                    .next()
                    .ok_or_else(|| "--region 需要 <rx> <rz>".to_owned())?
                    .parse::<i32>()
                    .map_err(|e| format!("rz 非法: {e}"))?;
                region = Some((rx, rz));
            }
            "--world" => {
                world = Some(PathBuf::from(
                    args.next().ok_or_else(|| "--world 需要路径".to_owned())?,
                ));
            }
            "--shm" => {
                shm = Some(args.next().ok_or_else(|| "--shm 需要名称".to_owned())?);
            }
            "--port" => {
                port = Some(
                    args.next()
                        .ok_or_else(|| "--port 需要端口".to_owned())?
                        .parse::<u16>()
                        .map_err(|e| format!("端口非法: {e}"))?,
                );
            }
            "--log-level" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--log-level 需要级别".to_owned())?;
                log_level = parse_log_level(&value)?;
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }

    let mode = if let Some((rx, rz)) = region {
        Launch::Region(RegionLaunchArgs {
            id: RegionId { rx, rz },
            world_dir: world.ok_or_else(|| "region 模式需要 --world".to_owned())?,
            shm_name: shm.unwrap_or_else(|| DEFAULT_SHM_NAME.to_owned()),
            log_level,
        })
    } else {
        Launch::Core(ServerLaunchArgs {
            address: port.map_or_else(
                || "0.0.0.0:25565".to_owned(),
                |port| format!("0.0.0.0:{port}"),
            ),
            world_dir: world.unwrap_or_else(|| PathBuf::from("world")),
            shm_name: shm.unwrap_or_else(|| DEFAULT_SHM_NAME.to_owned()),
            log_level,
        })
    };
    Ok(mode)
}

/// 启动方式。
enum Launch {
    /// core 模式。
    Core(ServerLaunchArgs),
    /// region 模式。
    Region(RegionLaunchArgs),
}

/// core 模式参数。
struct ServerLaunchArgs {
    address: String,
    world_dir: PathBuf,
    shm_name: String,
    log_level: LevelFilter,
}

impl Default for ServerLaunchArgs {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:25565".to_owned(),
            world_dir: PathBuf::from("world"),
            shm_name: DEFAULT_SHM_NAME.to_owned(),
            log_level: DEFAULT_LOG_LEVEL,
        }
    }
}

/// region 模式参数。
struct RegionLaunchArgs {
    id: RegionId,
    world_dir: PathBuf,
    shm_name: String,
    log_level: LevelFilter,
}

#[allow(clippy::print_stderr)] // 参数错误写入 stderr 是控制台惯例
fn usage_error(message: &str) -> ! {
    eprintln!("参数错误: {message}");
    eprintln!("用法: icbm [--world <目录>] [--port <端口>] [--shm <名称>] [--log-level <级别>]");
    eprintln!("      icbm --region <rx> <rz> --world <目录> [--shm <名称>] [--log-level <级别>]");
    eprintln!("级别: off | error | warn | info | debug | trace（默认 info）");
    std::process::exit(2);
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => usage_error(&message),
    };
    let (log_level, use_stderr) = match &args {
        Launch::Core(args) => (args.log_level, false),
        Launch::Region(args) => (args.log_level, true),
    };
    // region 子进程：日志走 stderr、不启动交互控制台（stdout 留给 core 控制台）。
    init_logging(log_level, use_stderr);

    match args {
        Launch::Region(args) => {
            debug!(
                id = %args.id.service_name(),
                "starting region worker mode"
            );
            // switch 鉴权令牌经环境变量传递（core 与 region 子进程共享同一环境）。
            let auth_token = std::env::var("ICBM_DATAROUTER_TOKEN")
                .ok()
                .filter(|token| !token.is_empty());
            let config = RegionConfig {
                id: args.id,
                world_dir: args.world_dir,
                shm_name: args.shm_name,
                auth_token,
            };
            if let Err(err) = icbm_region::run_region(config) {
                error!(%err, "region worker exited with error");
                std::process::exit(1);
            }
        }
        Launch::Core(args) => {
            info!("starting icbm core on {}", args.address);
            let config = icbm_core::config::ServerConfig {
                address: args.address,
                world_dir: args.world_dir,
                shm_name: args.shm_name,
                log_level: args.log_level,
                ..icbm_core::config::ServerConfig::default()
            };
            let server = match icbm_core::server::Server::new(config) {
                Ok(server) => server,
                Err(err) => {
                    error!(%err, "failed to initialize server");
                    std::process::exit(1);
                }
            };
            if let Err(err) = server.run() {
                error!(%err, "server exited with error");
                std::process::exit(1);
            }
            warn!("server stopped");
        }
    }
}
