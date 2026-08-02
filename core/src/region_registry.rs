//! region 注册表与子进程编排。
//!
//! core 负责 region 生命周期：拉起子进程（同一二进制 `--region` 模式）、
//! 处理注册/心跳、沉睡 30 秒后卸载、心跳超时标记失联。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use icbm_region::RegionId;
use tracing::{info, warn};

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
}

impl RegionEntry {
    fn new(pid: u32) -> Self {
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
        }
    }
}

/// region 注册表。
#[derive(Debug, Default)]
pub struct RegionRegistry {
    entries: HashMap<(i32, i32), RegionEntry>,
}

impl RegionRegistry {
    /// 新建空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

    /// 拉起 region 子进程（已存在则不重复拉起）。
    pub fn spawn(
        &mut self,
        id: &RegionId,
        world_dir: &std::path::Path,
        shm_name: &str,
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
            .arg(shm_name);
        let child = command.spawn()?;
        let pid = child.id();
        info!(id = %id.service_name(), pid, "spawned region process");
        self.entries.insert((id.rx, id.rz), RegionEntry::new(pid));
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
