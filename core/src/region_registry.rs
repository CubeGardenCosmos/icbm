//! region 注册表与子进程编排。
//!
//! core 负责 region 生命周期：拉起子进程（同一二进制 `--region` 模式）、
//! 处理注册/心跳、沉睡 30 秒后卸载、心跳超时标记失联。
//!
//! worker 进程池（`build/workspace/draft/worker_pool/README.md`）：
//! core 启动时拉起 `cpu_count - 2` 个常驻 worker（`--worker <id> --cpu <core>`），
//! region 由 worker 宿主（进程内线程），不再每 region 一个进程。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use icbm_region::RegionId;
use tracing::{debug, warn};
use tracing_subscriber::filter::LevelFilter;

/// region 运行信息。
#[derive(Debug, Clone)]
pub struct RegionEntry {
    /// 子进程 PID。
    pub pid: u32,
    /// 是否已注册（收到 REGISTER 且已 ACK）。
    pub alive: bool,
    /// 是否沉睡。
    pub sleeping: bool,
    /// 沉睡起始时间。
    pub sleeping_since: Option<Instant>,
    /// 最近心跳。
    pub last_heartbeat: Instant,
    /// 最近 TPS。
    pub tps: f32,
    /// 平均 tick 耗时（毫秒）。
    pub avg_tick_ms: f32,
    /// tick 计数。
    pub tick_count: u32,
    /// 已加载区块数。
    pub loaded_chunks: u32,
    /// 实体 ID 基址（core 分配；区域内实体 ID ∈ [base, base+999]，卸载时据此清理）。
    pub entity_base: i32,
    /// 所属 worker ID（`WORKER_ASSIGN` 分配；释放时经 worker `WORKER_RELEASE`）。
    pub worker_id: Option<u32>,
}

impl RegionEntry {
    fn new(pid: u32, entity_base: i32) -> Self {
        Self {
            pid,
            alive: false,
            sleeping: false,
            sleeping_since: None,
            last_heartbeat: Instant::now(),
            tps: 0.0,
            avg_tick_ms: 0.0,
            tick_count: 0,
            loaded_chunks: 0,
            entity_base,
            worker_id: None,
        }
    }
}

/// region 注册表。
#[derive(Debug, Default)]
pub struct RegionRegistry {
    entries: HashMap<(i32, i32), RegionEntry>,
    next_entity_base: i32,
}

/// 每个 region 的实体 ID 段宽度（core 按段分配，卸载时整段清除）。
pub const ENTITY_ID_STRIDE: i32 = 1000;

impl RegionRegistry {
    /// 新建空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            // 实体 ID 从 1000 起分配，避开玩家实体 ID（1）。
            next_entity_base: ENTITY_ID_STRIDE,
        }
    }

    /// 当前是否有活动或已注册的 region。
    #[must_use]
    pub fn contains(&self, id: &RegionId) -> bool {
        self.entries.contains_key(&(id.rx, id.rz))
    }

    /// 取 region 信息。
    #[must_use]
    pub fn get(&self, id: &RegionId) -> Option<&RegionEntry> {
        self.entries.get(&(id.rx, id.rz))
    }

    /// region 的实体 ID 基址。
    #[must_use]
    pub fn entity_base(&self, id: &RegionId) -> Option<i32> {
        self.get(id).map(|entry| entry.entity_base)
    }

    /// 全部 region ID。
    #[must_use]
    pub fn ids(&self) -> Vec<RegionId> {
        self.entries
            .keys()
            .map(|&(rx, rz)| RegionId { rx, rz })
            .collect()
    }

    /// 拉起 region 子进程（已存在则不重复拉起）。
    ///
    /// `log_level` 与 core 一致地传给子进程（region 经同一二进制 `--region` 入口，
    /// 在 `main` 中按 `--log-level` 初始化 tracing）。
    pub fn spawn(
        &mut self,
        id: &RegionId,
        world_dir: &std::path::Path,
        shm_name: &str,
        log_level: LevelFilter,
    ) -> Result<(), std::io::Error> {
        if let Some(entry) = self.entries.get(&(id.rx, id.rz))
            && entry.alive
        {
            return Ok(());
        }

        let exe = std::env::current_exe()?;
        let rx = id.rx.to_string();
        let rz = id.rz.to_string();
        let mut command = std::process::Command::new(exe);
        command
            .arg("--region")
            .arg(&rx)
            .arg(&rz)
            .arg("--world")
            .arg(world_dir)
            .arg("--shm")
            .arg(shm_name)
            .arg("--log-level")
            .arg(level_filter_str(log_level));
        let child = command.spawn()?;
        let pid = child.id();
        debug!(id = %id.service_name(), pid, "spawned region process");
        // 实体 ID 段分配：从 1000 起每 region 一段，避免与玩家实体 ID 冲突。
        let entity_base = self.next_entity_base;
        self.next_entity_base = self
            .next_entity_base
            .checked_add(ENTITY_ID_STRIDE)
            .unwrap_or(ENTITY_ID_STRIDE);
        self.entries
            .insert((id.rx, id.rz), RegionEntry::new(pid, entity_base));
        Ok(())
    }

    /// 标记注册（返回是否为新注册）。
    pub fn mark_registered(&mut self, id: &RegionId) -> bool {
        let Some(entry) = self.entries.get_mut(&(id.rx, id.rz)) else {
            return false;
        };
        let first = !entry.alive;
        entry.alive = true;
        entry.last_heartbeat = Instant::now();
        first
    }

    /// 记录 region → worker 归属（`ensure_region` 分配后、region 注册前调用；
    /// 无条目时先占位）。
    pub fn mark_assigned(&mut self, id: &RegionId, worker_id: u32) {
        if let Some(entry) = self.entries.get_mut(&(id.rx, id.rz)) {
            entry.worker_id = Some(worker_id);
            return;
        }
        let mut entry = RegionEntry::new(0, self.next_entity_base);
        entry.worker_id = Some(worker_id);
        self.next_entity_base = self
            .next_entity_base
            .checked_add(ENTITY_ID_STRIDE)
            .unwrap_or(ENTITY_ID_STRIDE);
        self.entries.insert((id.rx, id.rz), entry);
    }

    /// 更新心跳。
    pub fn update_heartbeat(
        &mut self,
        id: &RegionId,
        tps: f32,
        sleeping: bool,
        loaded: u32,
        ticks: u32,
    ) {
        let Some(entry) = self.entries.get_mut(&(id.rx, id.rz)) else {
            return;
        };
        let was_sleeping = entry.sleeping;
        entry.tps = tps;
        entry.loaded_chunks = loaded;
        entry.tick_count = ticks;
        entry.last_heartbeat = Instant::now();
        entry.sleeping = sleeping;
        if sleeping && !was_sleeping {
            entry.sleeping_since = Some(Instant::now());
        } else if !sleeping {
            entry.sleeping_since = None;
        }
    }

    /// 心跳超时的 region（失联）。
    #[must_use]
    pub fn timed_out(&self, now: Instant, timeout: Duration) -> Vec<RegionId> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.alive && now.duration_since(entry.last_heartbeat) > timeout)
            .map(|((rx, rz), _)| RegionId { rx: *rx, rz: *rz })
            .collect()
    }

    /// 沉睡超过宽限期的 region（应卸载）。
    #[must_use]
    pub fn sleep_unloads(&self, now: Instant, grace: Duration) -> Vec<RegionId> {
        self.entries
            .iter()
            .filter(|(_, entry)| {
                entry.alive
                    && entry.sleeping
                    && entry
                        .sleeping_since
                        .is_some_and(|since| now.duration_since(since) > grace)
            })
            .map(|((rx, rz), _)| RegionId { rx: *rx, rz: *rz })
            .collect()
    }

    /// 移除 region。
    pub fn remove(&mut self, id: &RegionId) {
        self.entries.remove(&(id.rx, id.rz));
    }

    /// 清空全部 region 记录（服务器关闭时）。
    pub fn clear_all(&mut self) {
        self.entries.clear();
    }

    /// 活动（未沉睡）与沉睡 region 数量。
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let sleeping = self.entries.values().filter(|e| e.sleeping).count();
        let alive = self.entries.values().filter(|e| e.alive).count();
        (alive - sleeping, sleeping)
    }

    /// 平均 TPS（活动 region）。
    #[must_use]
    pub fn avg_tps(&self) -> f32 {
        let entries: Vec<f32> = self
            .entries
            .values()
            .filter(|e| e.alive && !e.sleeping && e.tps > 0.0)
            .map(|e| e.tps)
            .collect();
        if entries.is_empty() {
            return 0.0;
        }
        entries.iter().sum::<f32>() / entries.len() as f32
    }

    /// 全部 region 数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 记录失联告警。
pub fn warn_timed_out(id: &RegionId) {
    warn!(id = %id.service_name(), "region heartbeat timeout, marked dead");
}

/// worker 进程池：core 启动时拉起的常驻 worker 列表与负载分配。
///
/// worker 是同一二进制 `--worker <id> --cpu <core>` 模式进程，注册
/// `icbm.worker.<id>`；core 把 region 经 `WORKER_ASSIGN` 分配给负载最低的
/// worker（region 合并：活跃 region 数 > worker 数时多个 region 共享一个
/// worker 进程）。region 卸载 = `WORKER_RELEASE`（worker 内移除，不杀进程）。
#[derive(Debug, Default)]
pub struct WorkerRegistry {
    /// worker ID → worker 信息。
    workers: HashMap<u32, WorkerEntry>,
    /// 每个 worker 当前负责的 region 数（负载）。
    load: HashMap<u32, usize>,
}

/// worker 进程信息。
#[derive(Debug, Clone)]
pub struct WorkerEntry {
    /// 子进程 PID。
    pub pid: u32,
    /// 绑定的 CPU 核心。
    pub cpu: u32,
    /// 最近注册时间。
    pub registered_at: Instant,
    /// 是否已注册（`WORKER_REGISTER`）。
    pub alive: bool,
}

impl WorkerRegistry {
    /// 新建空池。
    #[must_use]
    pub fn new() -> Self {
        Self {
            workers: HashMap::new(),
            load: HashMap::new(),
        }
    }

    /// 记录 worker 注册（`WORKER_REGISTER`），返回是否新注册。
    pub fn register(&mut self, id: u32, pid: u32, cpu: u32) -> bool {
        let first = !self.workers.contains_key(&id);
        self.workers.insert(
            id,
            WorkerEntry {
                pid,
                cpu,
                registered_at: Instant::now(),
                alive: true,
            },
        );
        self.load.entry(id).or_insert(0);
        first
    }

    /// 分配一个 region 到负载最低的 worker（region 合并）。
    /// 返回 `(worker_id, worker_service_name)`；无可用 worker 返回 `None`。
    ///
    /// 跳过已确认死亡的 worker 进程（进程存活探测失败；`kill(pid,0)`），
    /// 避免把 region 分配给死进程导致永不注册。
    #[must_use]
    pub fn assign_region(&mut self) -> Option<(u32, String)> {
        // 负载相同取 id 更小的 worker（确定性分配）
        let best = self
            .workers
            .iter()
            .filter(|(_, entry)| entry.alive)
            .filter(|(_, entry)| worker_process_alive(entry.pid))
            .min_by_key(|(id, _)| (self.load.get(id).copied().unwrap_or(0), **id))
            .map(|(id, _)| *id);
        let id = best?;
        *self.load.entry(id).or_insert(0) += 1;
        Some((id, format!("icbm.worker.{id}")))
    }

    /// 释放 region（负载 -1）。
    pub fn release_region(&mut self, worker_id: u32) {
        if let Some(load) = self.load.get_mut(&worker_id) {
            *load = load.saturating_sub(1);
        }
    }

    /// worker 数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.workers.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.workers.is_empty()
    }

    /// 全部 worker ID（升序，确定性）。
    #[must_use]
    pub fn ids(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.workers.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// 取 worker 信息。
    #[must_use]
    pub fn get(&self, id: u32) -> Option<&WorkerEntry> {
        self.workers.get(&id)
    }
}

/// `LevelFilter` → `--log-level` 参数值（供 region 子进程复用同一过滤级别）。
#[must_use]
pub const fn level_filter_str(level: LevelFilter) -> &'static str {
    match level {
        LevelFilter::OFF => "off",
        LevelFilter::ERROR => "error",
        LevelFilter::WARN => "warn",
        LevelFilter::INFO => "info",
        LevelFilter::DEBUG => "debug",
        LevelFilter::TRACE => "trace",
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerRegistry;

    #[test]
    fn assigns_to_least_loaded_worker() {
        let mut registry = WorkerRegistry::new();
        // pid 用当前进程（真实存活）；假 pid 会被 `worker_process_alive` 过滤
        let pid = std::process::id();
        registry.register(0, pid, 1);
        registry.register(1, pid, 2);
        // 初始负载都是 0 → 选 id 小的
        let (id, service) = registry.assign_region().unwrap();
        assert_eq!(id, 0);
        assert_eq!(service, "icbm.worker.0");
        // 现在 worker 0 负载 1 → 下一次选 worker 1
        let (id, _) = registry.assign_region().unwrap();
        assert_eq!(id, 1);
        // 释放 worker 1 → 负载回 0，下一次选 worker 1（负载 0 < worker0 的 1）
        registry.release_region(1);
        let (id, _) = registry.assign_region().unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn no_workers_returns_none() {
        let mut registry = WorkerRegistry::new();
        assert!(registry.assign_region().is_none());
    }

    #[test]
    fn worker_count_and_ids() {
        let mut registry = WorkerRegistry::new();
        registry.register(0, 100, 1);
        registry.register(1, 200, 2);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.ids(), vec![0, 1]);
        assert_eq!(registry.get(0).unwrap().pid, 100);
        assert_eq!(registry.get(1).unwrap().cpu, 2);
    }
}

/// worker 进程是否存活（`kill(pid,0)`；权限错误视为存活；pid=0 视为不可用）。
fn worker_process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: 仅查询信号 0（不发送）；pid 为 worker 子进程。
    let result = unsafe { libc::kill(i32::try_from(pid).unwrap_or(-1), 0) };
    result == 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
}
