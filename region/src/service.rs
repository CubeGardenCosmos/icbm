//! region 服务：注册 → 模拟 → 沉睡 → 卸载 的显式状态机与 20 TPS 循环。

use std::collections::HashSet;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use icbm_mc_767::codec::Writer;
use icbm_mc_767::packets::play::clientbound::{CAddEntity, CMoveEntityPos};
use icbm_mc_767::packets::play_ids;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::protocol::{
    CORE_SERVICE, DIM_OVERWORLD, Ipc, IpcError, IpcMessage, decode_block_update,
    decode_chunk_request, decode_player_enter, decode_player_leave, decode_register_ack,
    encode_chunk_data, encode_entity_data, encode_heartbeat, encode_register, encode_stats, kind,
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
    /// switch 鉴权令牌（`ICBM_DATAROUTER_TOKEN`），未启用时为 `None`。
    pub auth_token: Option<String>,
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

/// Creeper 实体类型注册表 ID（vanilla 1.21.1 `EntityType` 注册序，对照 master
/// `data/src/generated/entity_type.rs`：`CREEPER` = 23）。
const ENTITY_TYPE_CREEPER: i32 = 23;
/// 每区域最多存活的漫游 Creeper 数（实体 ID 取 `[entity_base, entity_base+12)`）。
const MAX_CREEPERS: usize = 12;
/// 每 tick 生成 Creeper 的概率（约 6 秒一只）。
const SPAWN_CHANCE: f64 = 1.0 / 120.0;
/// 每 tick 游走的概率。
const WANDER_CHANCE: f64 = 1.0 / 3.0;
/// Creeper 出生高度（flat 地表 y=3 之上）。
const CREEPER_SPAWN_Y: f64 = 4.0;

/// 区域内一只漫游 Creeper（不攻击，仅作 region 存活/卸载信号）。
struct Creeper {
    /// 实体 ID（`entity_base + 生成序`）。
    entity_id: i32,
    /// 实体 UUID。
    uuid: Uuid,
    /// X（方块）。
    x: f64,
    /// Z（方块）。
    z: f64,
    /// 偏航角（度）。
    yaw: f32,
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
    /// 待发送区块（每 tick 分批冲刷）。
    pending_chunks: std::collections::VecDeque<(Face, i32, i32)>,
    /// core 分配的实体 ID 基址（`REGISTER_ACK`）。
    entity_base: i32,
    /// 区域内漫游 Creeper（region 存活信号）。
    creepers: Vec<Creeper>,
    /// xorshift64 状态（种子取自区域坐标，固定种子可复现）。
    rng: u64,
}

impl RegionService {
    /// 每 tick 最多发送的区块数（8 × 约 42KB ≈ 336KB < 512KiB 单帧上限；
    /// 仍保留分批发送作流控，避免瞬时占满 channel 影响心跳）。
    const CHUNKS_PER_TICK: usize = 8;

    /// 应用 `setblock`/`fill` 方块修改并向区域玩家广播 `CBlockUpdate`。
    fn apply_block_update(
        &mut self,
        face: Face,
        x: i32,
        y: i32,
        z: i32,
        state_id: i32,
    ) -> Result<(), RegionError> {
        self.store.set_block(face, x, y, z, state_id)?;
        let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
            position: icbm_mc_767::packets::play::BlockPos::new(x, y, z),
            state_id,
        };
        self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
            packet.encode(w);
        })?;
        Ok(())
    }
    /// 新建服务（连接 datarouter、打开存档）。
    pub fn new(config: RegionConfig) -> Result<Self, RegionError> {
        let ipc = Ipc::connect(
            &config.shm_name,
            &config.id.service_name(),
            config.auth_token.clone(),
        )?;
        let store = ChunkStore::open(&config.world_dir, config.id)?;
        // 确定性 RNG 种子：区域坐标哈希（固定种子可复现，AGENTS 要求）。
        let rng = (u64::from(config.id.rx as u32)).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (u64::from(config.id.rz as u32)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        Ok(Self {
            config,
            ipc,
            store,
            state: RegionState::Registering,
            ticker: Ticker::new(),
            players: HashSet::new(),
            running: true,
            unknown_messages: 0,
            pending_chunks: std::collections::VecDeque::new(),
            entity_base: 0,
            creepers: Vec::new(),
            rng: rng | 1,
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
            let (dim, rx, rz, ok, entity_base) = decode_register_ack(&message.body)?;
            if dim == DIM_OVERWORLD && rx == self.config.id.rx && rz == self.config.id.rz {
                if ok {
                    self.entity_base = entity_base.unwrap_or(0);
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
        self.flush_chunks()?;
        // Phase 1 世界 tick：仅随机漫游 Creeper（region 存活信号）
        self.tick_creepers()?;
        let tps = self.ticker.end();
        if self.ticker.should_heartbeat() {
            self.send_heartbeat(tps);
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
        self.flush_chunks()?;
        if self.ticker.should_heartbeat() {
            self.send_heartbeat(0.0);
            self.ticker.mark_heartbeat();
        }
        std::thread::sleep(Duration::from_millis(100));
        if !self.players.is_empty() {
            info!(id = %self.config.id.service_name(), "player entered, waking up");
            self.state = RegionState::Simulating;
        }
        Ok(())
    }

    fn send_heartbeat(&self, tps: f32) {
        let sleeping = self.state == RegionState::Sleeping;
        // 心跳可短暂丢失：不致命，下一 tick 继续；避免 datarouter
        // 暂时性满/忙导致 region 进程退出。
        if let Err(err) = self.ipc.send_retry(
            CORE_SERVICE,
            kind::REGION_HEARTBEAT,
            &encode_heartbeat(
                tps,
                sleeping,
                self.store.loaded_count(),
                self.ticker.tick_count,
            ),
        ) {
            warn!(%err, "heartbeat send failed, skipped");
        }
    }

    /// 排空 IPC 消息队列并处理。
    ///
    /// 注意：`Frontend::recv` 会把超时 0 归一化为 `recv_timeout_ms`（默认 3000ms），
    /// `recv(0)` 会阻塞 3 秒并拖垮 20 TPS 循环；此处用 1ms 实现真正的非阻塞轮询。
    fn drain_messages(&mut self) -> Result<(), RegionError> {
        while let Some(message) = self.ipc.recv(1)? {
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
                // 把已存在的 Creeper 补发给新进入的玩家（客户端需 ADD_ENTITY 才渲染）
                self.resend_creepers()?;
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
                self.serve_chunks(face, cx, cz, radius);
            }
            kind::REGION_UNLOAD => {
                info!(id = %self.config.id.service_name(), "unload requested, flushing");
                self.store.flush()?;
                self.running = false;
            }
            kind::BLOCK_UPDATE => {
                let (dim, x, y, z, state_id) = decode_block_update(&message.body)?;
                let Some(face) = Face::from_byte(dim) else {
                    return Err(RegionError::InvalidMessage(format!("bad dim {dim}")));
                };
                self.apply_block_update(face, x, y, z, state_id)?;
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

    /// 响应区块请求（入队，由每 tick 的 `flush_chunks` 分批发送）。
    fn serve_chunks(&mut self, face: Face, cx: i32, cz: i32, radius: i32) {
        let radius = radius.clamp(0, 32);
        for dx in -radius..=radius {
            for dz in -radius..=radius {
                self.pending_chunks.push_back((face, cx + dx, cz + dz));
            }
        }
    }

    /// xorshift64 随机数。
    const fn next_random(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// [0,1) 均匀随机。
    fn rand_f64(&mut self) -> f64 {
        (self.next_random() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 随机漫游 Creeper：随机生成新个体并让已有个体游走。
    fn tick_creepers(&mut self) -> Result<(), RegionError> {
        if self.creepers.len() < MAX_CREEPERS && self.rand_f64() < SPAWN_CHANCE {
            self.spawn_creeper()?;
        }
        let min_x = f64::from(self.config.id.rx) * 512.0;
        let min_z = f64::from(self.config.id.rz) * 512.0;
        // 先预掷随机数再逐个体决策，避免同时借用 self.rng 与 self.creepers。
        let rolls: Vec<(bool, u64)> = (0..self.creepers.len())
            .map(|_| (self.rand_f64() < WANDER_CHANCE, self.next_random() % 4))
            .collect();
        let mut moves: Vec<(i32, i16, i16)> = Vec::new();
        for (creeper, (wander, dir)) in self.creepers.iter_mut().zip(rolls) {
            if !wander {
                continue;
            }
            let (dx, dz) = match dir {
                0 => (1, 0),
                1 => (-1, 0),
                2 => (0, 1),
                _ => (0, -1),
            };
            let next_x = creeper.x + f64::from(dx);
            let next_z = creeper.z + f64::from(dz);
            // 不越出本区域 512×512 方块边界（区域外归相邻 region 模拟）
            if !(min_x..min_x + 512.0).contains(&next_x)
                || !(min_z..min_z + 512.0).contains(&next_z)
            {
                continue;
            }
            creeper.x = next_x;
            creeper.z = next_z;
            moves.push((creeper.entity_id, (dx * 4096) as i16, (dz * 4096) as i16));
        }
        for (entity_id, delta_x, delta_z) in moves {
            let move_pos = CMoveEntityPos {
                entity_id,
                delta_x,
                delta_y: 0,
                delta_z,
                on_ground: true,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_MOVE_ENTITY_POS, |w| {
                move_pos.encode(w);
            })?;
        }
        Ok(())
    }

    /// 在区域内随机位置生成一只 Creeper 并广播 `ADD_ENTITY`。
    fn spawn_creeper(&mut self) -> Result<(), RegionError> {
        let index = self.creepers.len();
        let entity_id = self.entity_base + i32::try_from(index).unwrap_or(i32::MAX);
        let cx = self.config.id.rx * 32 + (self.next_random() % 32) as i32;
        let cz = self.config.id.rz * 32 + (self.next_random() % 32) as i32;
        let creeper = Creeper {
            entity_id,
            uuid: Uuid::new_v4(),
            x: f64::from(cx * 16) + self.rand_f64() * 16.0,
            z: f64::from(cz * 16) + self.rand_f64() * 16.0,
            yaw: (self.rand_f64() * 360.0) as f32,
        };
        self.send_entity_packet(play_ids::CLIENTBOUND_ADD_ENTITY, |w| {
            CAddEntity {
                entity_id: creeper.entity_id,
                uuid: creeper.uuid,
                entity_type: ENTITY_TYPE_CREEPER,
                x: creeper.x,
                y: CREEPER_SPAWN_Y,
                z: creeper.z,
                pitch: 0.0,
                yaw: creeper.yaw,
            }
            .encode(w);
        })?;
        debug!(entity_id, "creeper spawned");
        self.creepers.push(creeper);
        Ok(())
    }

    /// 玩家进入后补发所有已有 Creeper 的 `ADD_ENTITY`。
    fn resend_creepers(&self) -> Result<(), RegionError> {
        for creeper in &self.creepers {
            let add = CAddEntity {
                entity_id: creeper.entity_id,
                uuid: creeper.uuid,
                entity_type: ENTITY_TYPE_CREEPER,
                x: creeper.x,
                y: CREEPER_SPAWN_Y,
                z: creeper.z,
                pitch: 0.0,
                yaw: creeper.yaw,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_ADD_ENTITY, |w| add.encode(w))?;
        }
        Ok(())
    }

    /// 经 `ENTITY_DATA` 把 clientbound 包交给 core 转发给本区域玩家。
    fn send_entity_packet(
        &self,
        packet_id: i32,
        encode: impl FnOnce(&mut Writer),
    ) -> Result<(), RegionError> {
        let mut writer = Writer::new();
        encode(&mut writer);
        let payload = writer.into_bytes();
        self.ipc.send_retry(
            CORE_SERVICE,
            kind::ENTITY_DATA,
            &encode_entity_data(
                DIM_OVERWORLD,
                self.config.id.rx,
                self.config.id.rz,
                packet_id,
                &payload,
            ),
        )?;
        Ok(())
    }

    /// 每 tick 最多发送固定数量的区块，避免一次性灌满 datarouter channel
    /// （81 区块约 4.2MB > channel 容量会导致 send BUSY 阻塞心跳）。
    fn flush_chunks(&mut self) -> Result<(), RegionError> {
        let mut sent = 0;
        let started = std::time::Instant::now();
        while sent < Self::CHUNKS_PER_TICK {
            let Some((face, chunk_x, chunk_z)) = self.pending_chunks.pop_front() else {
                break;
            };
            let payload = self.store.network_chunk_payload(face, chunk_x, chunk_z)?;
            self.ipc.send_retry(
                CORE_SERVICE,
                kind::CHUNK_DATA,
                &encode_chunk_data(face.byte(), chunk_x, chunk_z, &payload),
            )?;
            sent += 1;
        }
        if sent > 0 {
            debug!(
                sent,
                elapsed_ms = started.elapsed().as_millis(),
                pending = self.pending_chunks.len(),
                "chunk flush batch"
            );
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
