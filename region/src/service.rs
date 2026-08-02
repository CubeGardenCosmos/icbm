//! region 服务：注册 → 模拟 → 沉睡 → 卸载 的显式状态机与 20 TPS 循环。

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::protocol::{
    CORE_SERVICE, DIM_OVERWORLD, Ipc, IpcError, IpcMessage, decode_chunk_request,
    decode_player_enter, decode_player_leave, decode_register_ack, encode_chunk_data,
    encode_heartbeat, encode_register, encode_stats, kind,
};
use crate::store::ChunkStore;
use crate::{Face, RegionId};

/// region 配置。
#[derive(Debug, Clone)]
pub struct RegionConfig {
    /// 区域 ID。
    pub id: RegionId,
    /// 世界目录（含 region/ 子目录）。
    pub world_dir: PathBuf,
    /// datarouter 共享内存名。
    pub shm_name: String,
}

/// region 服务错误。
#[derive(Debug)]
pub enum RegionError {
    /// IPC 错误。
    Ipc(IpcError),
    /// 存储错误。
    Store(crate::store::StoreError),
    /// 注册被拒绝。
    RegistrationRejected,
    /// 收到无效消息。
    InvalidMessage(String),
}

impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ipc(err) => write!(f, "ipc: {err}"),
            Self::Store(err) => write!(f, "store: {err}"),
            Self::RegistrationRejected => f.write_str("registration rejected by core"),
            Self::InvalidMessage(msg) => write!(f, "invalid message: {msg}"),
        }
    }
}

impl std::error::Error for RegionError {}

impl From<IpcError> for RegionError {
    fn from(value: IpcError) -> Self {
        Self::Ipc(value)
    }
}

impl From<crate::store::StoreError> for RegionError {
    fn from(value: crate::store::StoreError) -> Self {
        Self::Store(value)
    }
}

/// 区域生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionState {
    /// 已连 datarouter，等待 core 确认。
    Registering,
    /// 正常模拟（20 TPS）。
    Simulating,
    /// 无玩家，停止 tick（保留进程，响应 IPC）。
    Sleeping,
}

/// 20 TPS 节拍与 TPS 测量。
#[derive(Debug)]
struct Ticker {
    interval: Duration,
    tick_start: Instant,
    last_ticks: Vec<f64>,
    tick_count: u64,
    last_heartbeat: Instant,
}

impl Ticker {
    const TICK_INTERVAL_MS: u64 = 50;
    const TPS_WINDOW: usize = 20;
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);

    fn new() -> Self {
        let now = Instant::now();
        Self {
            interval: Duration::from_millis(Self::TICK_INTERVAL_MS),
            tick_start: now,
            last_ticks: Vec::with_capacity(Self::TPS_WINDOW),
            tick_count: 0,
            last_heartbeat: now,
        }
    }

    fn begin(&mut self) {
        self.tick_start = Instant::now();
    }

    /// 记录本 tick 耗时并返回当前 TPS（0..20 钳制）。
    fn end(&mut self) -> f32 {
        let elapsed = self.tick_start.elapsed().as_secs_f64() * 1000.0;
        self.last_ticks.push(elapsed);
        if self.last_ticks.len() > Self::TPS_WINDOW {
            self.last_ticks.remove(0);
        }
        self.tick_count += 1;
        let avg = self.last_ticks.iter().sum::<f64>() / self.last_ticks.len() as f64;
        if avg <= 0.0 {
            return 20.0;
        }
        (1000.0 / avg).clamp(0.0, 20.0) as f32
    }

    /// 平均 tick 耗时（毫秒）。
    fn avg_tick_ms(&self) -> f32 {
        if self.last_ticks.is_empty() {
            return 0.0;
        }
        (self.last_ticks.iter().sum::<f64>() / self.last_ticks.len() as f64) as f32
    }

    /// 等待下一个 tick 边界。
    fn wait_next(&self) {
        let elapsed = self.tick_start.elapsed();
        if let Some(remaining) = self.interval.checked_sub(elapsed) {
            std::thread::sleep(remaining);
        }
    }

    fn should_heartbeat(&self) -> bool {
        self.last_heartbeat.elapsed() >= Self::HEARTBEAT_INTERVAL
    }

    fn mark_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }
}

/// region 服务。
pub struct RegionService {
    config: RegionConfig,
    ipc: Ipc,
    store: ChunkStore,
    state: RegionState,
    ticker: Ticker,
    players: HashSet<Uuid>,
    running: bool,
    unknown_messages: u64,
}

impl RegionService {
    /// 新建服务（连接 datarouter、打开存档）。
    pub fn new(config: RegionConfig) -> Result<Self, RegionError> {
        let ipc = Ipc::connect(&config.shm_name, &config.id.service_name())?;
        let store = ChunkStore::open(&config.world_dir, config.id)?;
        Ok(Self {
            config,
            ipc,
            store,
            state: RegionState::Registering,
            ticker: Ticker::new(),
            players: HashSet::new(),
            running: true,
            unknown_messages: 0,
        })
    }

    /// 当前状态。
    #[must_use]
    pub const fn state(&self) -> RegionState {
        self.state
    }

    /// 运行主循环直至卸载。
    pub fn run(&mut self) -> Result<(), RegionError> {
        self.register()?;
        while self.running {
            match self.state {
                RegionState::Registering => return Err(RegionError::RegistrationRejected),
                RegionState::Simulating => self.tick_simulate()?,
                RegionState::Sleeping => self.tick_sleeping()?,
            }
        }
        info!(id = %self.config.id.service_name(), "region unloaded");
        Ok(())
    }

    /// 注册并等待 ACK。
    fn register(&mut self) -> Result<(), RegionError> {
        let pid = std::process::id();
        self.ipc.send(
            CORE_SERVICE,
            kind::REGION_REGISTER,
            &encode_register(DIM_OVERWORLD, self.config.id.rx, self.config.id.rz, pid),
        )?;
        info!(
            id = %self.config.id.service_name(),
            pid, "region registering with core"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let Some(message) = self.ipc.recv(1000)? else {
                continue;
            };
            if message.kind != kind::REGISTER_ACK {
                debug!(kind = message.kind, "ignoring message during registration");
                continue;
            }
            let (dim, rx, rz, ok) = decode_register_ack(&message.body)?;
            if dim == DIM_OVERWORLD && rx == self.config.id.rx && rz == self.config.id.rz {
                if ok {
                    self.state = RegionState::Simulating;
                    info!(id = %self.config.id.service_name(), "region registered");
                    return Ok(());
                }
                return Err(RegionError::RegistrationRejected);
            }
        }
        Err(RegionError::RegistrationRejected)
    }

    /// 模拟 tick。
    fn tick_simulate(&mut self) -> Result<(), RegionError> {
        self.ticker.begin();
        self.drain_messages()?;
        // Phase 1 世界 tick 为空（无实体/方块更新），仅维持 TPS 与心跳
        let tps = self.ticker.end();
        if self.ticker.should_heartbeat() {
            self.send_heartbeat(tps)?;
            self.ticker.mark_heartbeat();
        }
        self.ticker.wait_next();
        if self.players.is_empty() {
            info!(id = %self.config.id.service_name(), "no players, sleeping");
            self.state = RegionState::Sleeping;
        }
        Ok(())
    }

    /// 沉睡：不 tick 世界，保持响应与心跳。
    fn tick_sleeping(&mut self) -> Result<(), RegionError> {
        self.drain_messages()?;
        if self.ticker.should_heartbeat() {
            self.send_heartbeat(0.0)?;
            self.ticker.mark_heartbeat();
        }
        std::thread::sleep(Duration::from_millis(100));
        if !self.players.is_empty() {
            info!(id = %self.config.id.service_name(), "player entered, waking up");
            self.state = RegionState::Simulating;
        }
        Ok(())
    }

    fn send_heartbeat(&self, tps: f32) -> Result<(), RegionError> {
        let sleeping = self.state == RegionState::Sleeping;
        self.ipc.send(
            CORE_SERVICE,
            kind::REGION_HEARTBEAT,
            &encode_heartbeat(
                tps,
                sleeping,
                self.store.loaded_count(),
                self.ticker.tick_count,
            ),
        )?;
        Ok(())
    }

    /// 排空 IPC 消息队列并处理。
    fn drain_messages(&mut self) -> Result<(), RegionError> {
        while let Some(message) = self.ipc.recv(0)? {
            self.handle_message(&message)?;
            if !self.running {
                return Ok(());
            }
        }
        Ok(())
    }

    fn handle_message(&mut self, message: &IpcMessage) -> Result<(), RegionError> {
        match message.kind {
            kind::PLAYER_ENTER => {
                let (uuid, _, _) = decode_player_enter(&message.body)?;
                self.players.insert(uuid);
                debug!(%uuid, "player entered region");
            }
            kind::PLAYER_LEAVE => {
                let uuid = decode_player_leave(&message.body)?;
                self.players.remove(&uuid);
                debug!(%uuid, "player left region");
            }
            kind::CHUNK_REQUEST => {
                let (dim, cx, cz, radius) = decode_chunk_request(&message.body)?;
                let Some(face) = Face::from_byte(dim) else {
                    return Err(RegionError::InvalidMessage(format!("bad dim {dim}")));
                };
                self.serve_chunks(face, cx, cz, radius)?;
            }
            kind::REGION_UNLOAD => {
                info!(id = %self.config.id.service_name(), "unload requested, flushing");
                self.store.flush()?;
                self.running = false;
            }
            kind::REGION_STATS => {
                let tps = if self.state == RegionState::Simulating {
                    (1000.0 / self.ticker.avg_tick_ms().max(0.001)).clamp(0.0, 20.0)
                } else {
                    0.0
                };
                self.ipc.send(
                    CORE_SERVICE,
                    kind::REGION_STATS_RESP,
                    &encode_stats(
                        tps,
                        self.ticker.avg_tick_ms(),
                        u32::try_from(self.ticker.tick_count).unwrap_or(u32::MAX),
                        self.state == RegionState::Sleeping,
                        self.store.loaded_count(),
                    ),
                )?;
            }
            other => {
                self.unknown_messages += 1;
                warn!(kind = other, "unhandled message kind");
            }
        }
        Ok(())
    }

    /// 响应区块请求（含半径内邻居）。
    fn serve_chunks(
        &mut self,
        face: Face,
        cx: i32,
        cz: i32,
        radius: i32,
    ) -> Result<(), RegionError> {
        let radius = radius.clamp(0, 32);
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                let chunk_x = cx + dx;
                let chunk_z = cz + dz;
                let payload = self.store.network_chunk_payload(face, chunk_x, chunk_z)?;
                self.ipc.send(
                    CORE_SERVICE,
                    kind::CHUNK_DATA,
                    &encode_chunk_data(face.byte(), chunk_x, chunk_z, &payload),
                )?;
            }
        }
        Ok(())
    }
}

/// 运行 region（core 二进制 `--region` 模式的入口）。
pub fn run_region(config: RegionConfig) -> Result<(), RegionError> {
    let mut service = RegionService::new(config)?;
    match service.run() {
        Ok(()) => Ok(()),
        Err(err) => {
            error!(%err, "region exited with error");
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Ticker;

    #[test]
    fn ticker_measures_tps() {
        let mut ticker = Ticker::new();
        for _ in 0..30 {
            ticker.begin();
            std::thread::sleep(std::time::Duration::from_millis(5));
            let tps = ticker.end();
            assert!((10.0..=20.0).contains(&tps), "tps was {tps}");
        }
        assert!(ticker.tick_count >= 30);
    }
}
