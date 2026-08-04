//! worker 进程宿主：常驻进程内运行多个 region 模拟线程。
//!
//! 架构（见 `build/workspace/draft/worker_pool/README.md`）：
//! - worker 是 core 拉起的常驻子进程（同一二进制 `--worker` 模式），
//!   注册 `icbm.worker.<id>` 服务名，接收 core 的 `WORKER_ASSIGN` /
//!   `WORKER_RELEASE` 编排消息；
//! - 每个 region 一个独立线程运行现有 [`run_region`]（复用单 region 主循环：
//!   注册 `icbm.region.<id>`、20 TPS tick、IPC 处理），region 之间互不干扰，
//!   各自持有独立 datarouter 连接与 `ChunkStore`——符合"每 region 单线程
//!   tick 所有权边界"（进程只是容器 + CPU 绑核单位，不引入进程内并行模拟）；
//! - region 卸载 = 停止其线程并移除（进程存活），不再 spawn/kill 进程，
//!   消除"flush 超时被杀"与"spawn/remove 竞态注册被拒"故障；
//! - CPU 绑核：worker 启动时按 `--cpu` 用 `sched_setaffinity` 绑定到指定核心
//!   （core 除外）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::{debug, error, info, warn};

use crate::protocol::{CORE_SERVICE, Ipc, decode_worker_region, encode_worker_register, kind};
use crate::service::RegionService;
use crate::{RegionConfig, RegionError, RegionId};

/// worker 配置。
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// worker ID（0 起）。
    pub id: u32,
    /// 世界目录（传给被分配的 region）。
    pub world_dir: std::path::PathBuf,
    /// datarouter 共享内存名。
    pub shm_name: String,
    /// switch 鉴权令牌。
    pub auth_token: Option<String>,
    /// 绑定的 CPU 核心（`None` = 不绑核）。
    pub cpu: Option<u32>,
}

/// 一个 region 的宿主状态。
struct HostedRegion {
    /// 停止标志（`WORKER_RELEASE` 时置位）。
    stop: Arc<AtomicBool>,
}

/// worker 宿主：持有 datarouter 连接并管理多个 region 线程。
pub struct WorkerHost {
    config: WorkerConfig,
    ipc: Ipc,
    /// worker 服务名（`icbm.worker.<id>`）。
    service_name: String,
    /// 已分配 region（区域 → 停止标志）。
    regions: Mutex<HashMap<(i32, i32), HostedRegion>>,
    running: Arc<AtomicBool>,
}

impl WorkerHost {
    /// 连接 datarouter 并注册 `icbm.worker.<id>`，上报 CPU 绑定后进入
    /// 编排循环（分配/释放 region），直至收到卸载。
    pub fn run(config: WorkerConfig) -> Result<(), WorkerError> {
        let service_name = format!("icbm.worker.{}", config.id);
        let ipc = Ipc::connect(&config.shm_name, &service_name, config.auth_token.clone())
            .map_err(WorkerError::Ipc)?;
        // CPU 绑核（core 除外；`--cpu` 指定）
        if let Some(cpu) = config.cpu {
            bind_cpu(cpu)?;
            info!(id = %service_name, cpu, "worker bound to cpu");
        }
        let host = Self {
            service_name: service_name.clone(),
            ipc,
            config,
            regions: Mutex::new(HashMap::new()),
            running: Arc::new(AtomicBool::new(true)),
        };
        // 上报注册（core 据此记录 worker 与 CPU 绑定）
        let pid = std::process::id();
        host.ipc
            .send_retry(
                CORE_SERVICE,
                kind::WORKER_REGISTER,
                &encode_worker_register(pid, host.config.cpu.unwrap_or(0) as u8),
            )
            .map_err(WorkerError::Ipc)?;
        info!(id = %service_name, pid, "worker registered");
        host.loop_forever()
    }

    /// 编排循环：接收 `WORKER_ASSIGN` / `WORKER_RELEASE`，管理 region 线程。
    fn loop_forever(&self) -> Result<(), WorkerError> {
        while self.running.load(Ordering::Relaxed) {
            let Some(message) = self.ipc.recv(1000).map_err(WorkerError::Ipc)? else {
                continue;
            };
            match message.kind {
                kind::WORKER_ASSIGN => {
                    let Ok((rx, rz)) = decode_worker_region(&message.body) else {
                        warn!("malformed worker assign");
                        continue;
                    };
                    self.assign(RegionId { rx, rz })?;
                }
                kind::WORKER_RELEASE => {
                    let Ok((rx, rz)) = decode_worker_region(&message.body) else {
                        warn!("malformed worker release");
                        continue;
                    };
                    self.release(RegionId { rx, rz });
                }
                other => {
                    debug!(kind = other, "ignoring message in worker loop");
                }
            }
        }
        Ok(())
    }

    /// 分配一个 region：若未在本 worker 内启动，则拉起 region 模拟线程。
    fn assign(&self, id: RegionId) -> Result<(), WorkerError> {
        {
            let regions = self.regions.lock().expect("regions lock");
            if regions.contains_key(&(id.rx, id.rz)) {
                debug!(id = %id.service_name(), "region already hosted");
                return Ok(());
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop);
        let config = RegionConfig {
            id,
            world_dir: self.config.world_dir.clone(),
            shm_name: self.config.shm_name.clone(),
            auth_token: self.config.auth_token.clone(),
        };
        // region 线程：复用单 region 主循环；`running` 置 false 时正常退出
        let thread = std::thread::Builder::new()
            .name(id.service_name())
            .spawn(move || {
                let mut service = match RegionService::new(config) {
                    Ok(service) => service,
                    Err(err) => {
                        error!(%err, id = %id.service_name(), "region init failed");
                        return;
                    }
                };
                if let Err(err) = service.run_until(stop_clone.as_ref()) {
                    error!(%err, id = %id.service_name(), "region exited with error");
                }
            })
            .map_err(WorkerError::Io)?;
        // 线程句柄随 region 移除而 join；此处只记录停止标志
        self.regions
            .lock()
            .expect("regions lock")
            .insert((id.rx, id.rz), HostedRegion { stop });
        info!(id = %id.service_name(), "region assigned to worker {}", self.config.id);
        let _ = thread; // 生命周期由停止标志 + core 心跳/超时兜底
        Ok(())
    }

    /// 释放一个 region：置停止标志（region 线程 flush 后退出），进程存活。
    fn release(&self, id: RegionId) {
        let Some(hosted) = self
            .regions
            .lock()
            .expect("regions lock")
            .remove(&(id.rx, id.rz))
        else {
            debug!(id = %id.service_name(), "region not hosted, ignoring release");
            return;
        };
        hosted.stop.store(true, Ordering::Relaxed);
        info!(id = %id.service_name(), "region released from worker {}", self.config.id);
    }
}

/// worker 错误。
#[derive(Debug)]
pub enum WorkerError {
    /// IPC 错误。
    Ipc(crate::protocol::IpcError),
    /// IO 错误（线程拉起等）。
    Io(std::io::Error),
    /// CPU 绑核失败。
    CpuBind(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipc(err) => write!(f, "ipc: {err}"),
            Self::Io(err) => write!(f, "io: {err}"),
            Self::CpuBind(msg) => write!(f, "cpu bind: {msg}"),
        }
    }
}

impl std::error::Error for WorkerError {}

/// 用 `sched_setaffinity` 把当前进程绑定到指定 CPU 核心。
///
/// `cpu` 超过可用核心数时回退到 `cpu % nproc`（不报错），避免配置错误
/// 导致 worker 无法启动。
fn bind_cpu(cpu: u32) -> Result<(), WorkerError> {
    let nproc = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let cpu = cpu % nproc;
    // SAFETY: 固定大小 CPU 集合；`sched_setaffinity` 的返回值经检查。
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::CPU_SET(cpu as usize, &mut set);
        // SAFETY: set 为栈上有效 cpu_set_t，size 匹配，返回值经检查
        let ret = libc::sched_setaffinity(
            0,
            std::mem::size_of::<libc::cpu_set_t>(),
            std::ptr::addr_of!(set),
        );
        if ret != 0 {
            let err = std::io::Error::last_os_error();
            return Err(WorkerError::CpuBind(format!(
                "sched_setaffinity(cpu={cpu}) failed: {err}"
            )));
        }
    }
    Ok(())
}
