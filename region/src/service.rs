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
    CORE_SERVICE, DIM_OVERWORLD, Ipc, IpcError, IpcMessage, decode_block_nbt_get,
    decode_block_nbt_set, decode_block_query, decode_block_raycast, decode_block_update,
    decode_chunk_request, decode_gamerule_update, decode_player_enter, decode_player_leave,
    decode_player_move, decode_portal_create, decode_register_ack, decode_surface_query,
    decode_time_update, decode_weather_update, encode_block_nbt_resp, encode_block_query_resp,
    encode_block_raycast_resp, encode_chunk_data, encode_command_execute, encode_entity_data,
    encode_heartbeat, encode_register, encode_stats, encode_surface_resp, kind,
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
/// 每区域最多存活的 Creeper 数（vanilla 每区域怪物上限约 70，本实现取小）。
const MAX_CREEPERS: usize = 12;
/// 每 tick 生成 Creeper 的概率（夜晚约 2 秒一只；vanilla 每区块多次生成尝试，
/// 取较高值保证测试与演示可复现）。
const SPAWN_CHANCE: f64 = 1.0 / 40.0;
/// Creeper 出生高度（superflat 地表顶 -61 之上）。
const CREEPER_SPAWN_Y: f64 = crate::OVERWORLD_SURFACE_FEET_Y as f64;
/// 敌对生物追踪/引爆半径（方块）：vanilla `CreeperEntity` 的
/// `CHARGED_CREEPER`/`FUSE_TIME` 与 `MobEntity` 目标半径。
const CREEPER_TARGET_RANGE: f64 = 16.0;
/// 点燃引信的玩家距离（vanilla 3 格内开始闪烁）。
const CREEPER_FUSE_RANGE: f64 = 3.0;
/// 引信时长（tick；vanilla `CreeperEntity.FUSE_TIME` = 30）。
const CREEPER_FUSE_TICKS: i32 = 30;
/// 夜晚时间窗（世界时间 % 24000；vanilla 13000..23000 为夜间，敌对生物生成）。
const NIGHT_START: i64 = 13_000;
const NIGHT_END: i64 = 23_000;

/// 区域内一只漫游 Creeper（不攻击，仅作 region 存活/卸载信号）。
struct Creeper {
    /// 实体 ID（`entity_base + 生成序`）。
    entity_id: i32,
    /// 实体 UUID。
    uuid: Uuid,
    /// X（方块）。
    x: f64,
    /// Y（方块）。
    y: f64,
    /// Z（方块）。
    z: f64,
    /// 偏航角（度）。
    yaw: f32,
    /// 上次移动方向（1/4 方向索引；避免连续折返，寻路更自然）。
    last_dir: u8,
    /// 引信倒计时（tick；-1 = 未点燃）。点燃后闪烁并倒计时爆炸（vanilla 30 tick）。
    fuse: i32,
}

/// 传送门框架检测结果：`(朝向, 待填充方块列表)`。
type FoundFrame = (crate::portal::PortalAxis, Vec<(i32, i32, i32)>);

/// region 服务。
pub struct RegionService {
    config: RegionConfig,
    ipc: Ipc,
    store: ChunkStore,
    state: RegionState,
    ticker: Ticker,
    players: HashSet<Uuid>,
    /// 玩家最新位置（UUID → xyz；敌对生物追踪用，`PLAYER_MOVE` 更新）。
    player_positions: std::collections::HashMap<Uuid, (f64, f64, f64)>,
    /// 玩家所在区块缓存（`PLAYER_MOVE` 节流用：区块变化才转发）。
    player_chunks: std::collections::HashMap<Uuid, (i32, i32)>,
    running: bool,
    unknown_messages: u64,
    /// 待发送区块（每 tick 分批冲刷）。
    pending_chunks: std::collections::VecDeque<(Face, i32, i32)>,
    /// core 分配的实体 ID 基址（`REGISTER_ACK`）。
    entity_base: i32,
    /// 区域内非玩家实体（掉落物/TNT）。
    entities: crate::entities::EntityManager,
    /// 区域内友好生物（牛/猪/鸡/羊）。
    mobs: crate::mobs::MobManager,
    /// 区域内漫游 Creeper（region 存活信号）。
    creepers: Vec<Creeper>,
    /// xorshift64 状态（种子取自区域坐标，固定种子可复现）。
    rng: u64,
    /// 世界游戏规则（`GAMERULE_UPDATE` 同步；默认原版值）。
    gamerules: std::collections::HashMap<String, String>,
    /// 世界时间（tick，昼夜循环用）。
    world_time: i64,
    /// 上次命令方块执行状态（坐标 → 上次是否已触发；防止每 tick 重复执行）。
    command_block_fired: std::collections::HashMap<(i32, i32, i32), bool>,
    /// 活塞供电快照（坐标 → 上次是否供电；边沿检测用）。
    piston_powered: std::collections::HashMap<(i32, i32, i32), bool>,
    /// 上次广播时间（节流：每秒一次）。
    last_time_broadcast: Instant,
    /// 天气状态（雨/雷/梯度）。
    weather: crate::weather::WeatherState,
    /// 流体计划 tick 队列（vanilla `LevelTicks<Fluid>`；放置/邻居变化 →
    /// `scheduleTick(pos, fluid, getTickDelay)`，到期执行 `FlowingFluid.tick`）。
    fluid_ticks: crate::ticks::LevelTicks<crate::fluids::FluidKind>,
    /// 计划 tick 调度顺序计数器（vanilla `subTickOrder`：同 tick 同优先级 FIFO）。
    sub_tick_order: u64,
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
        use icbm_mc_767::packets::play::block_state_ids;
        self.store.set_block(face, x, y, z, state_id)?;
        // 放置黑曜石：检测是否构成 4×5 传送门框架，构成则填充传送门方块
        if face == Face::Overworld && state_id == block_state_ids::OBSIDIAN {
            self.try_build_portal(x, y, z)?;
        }
        // TNT 方块放置：生成 TNT 实体（`minecraft:tnt`），不保留方块（vanilla
        // 放置即点火；此处放置即视为已放置方块并生成实体）
        if face == Face::Overworld && state_id == 2095 {
            let entity_id = self.entity_base
                + 1000
                + i32::try_from(self.entities.tnts.len()).unwrap_or(i32::MAX);
            self.entities.spawn_tnt(
                entity_id,
                f64::from(x) + 0.5,
                f64::from(y) + 0.5,
                f64::from(z) + 0.5,
            );
            let tnt = self.entities.tnts.last().expect("just spawned").clone();
            let add = icbm_mc_767::packets::play::clientbound::CAddEntity {
                entity_id: tnt.entity_id,
                uuid: tnt.uuid,
                entity_type: crate::entities::entity_type_ids::TNT,
                x: tnt.x,
                y: tnt.y,
                z: tnt.z,
                pitch: 0.0,
                yaw: 0.0,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_ADD_ENTITY, |w| {
                add.encode(w);
            })?;
            return Ok(());
        }
        let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
            position: icbm_mc_767::packets::play::BlockPos::new(x, y, z),
            state_id,
        };
        self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
            packet.encode(w);
        })?;
        Ok(())
    }

    /// 维度切换目的端生成传送门（vanilla `PortalForcer.createPortal`）：
    /// 以玩家落点 (x, y, z) 为中心建 4×5 黑曜石框架并填充传送门方块，
    /// 向区域玩家广播 `CBlockUpdate`。
    fn create_portal(&mut self, face: Face, x: i32, y: i32, z: i32) -> Result<(), RegionError> {
        use crate::portal::{PortalAxis, build_portal, portal_state};
        // 朝向确定性选择（坐标奇偶），与 core `portal_center` 落点一致
        let axis = PortalAxis::from_pos(x, z);
        let bottom_y = y - 2;
        let (frame, fill) = build_portal(x, z, bottom_y, axis);
        let obsidian = icbm_mc_767::packets::play::block_state_ids::OBSIDIAN;
        let state = portal_state(axis);
        let mut updates: Vec<(i32, i32, i32, i32)> = Vec::with_capacity(frame.len() + fill.len());
        // 边界方块非本区域所有时跳过（set_block 返回 NotOwned）
        for (fx, fy, fz) in frame {
            if self.store.set_block(face, fx, fy, fz, obsidian).is_ok() {
                updates.push((fx, fy, fz, obsidian));
            }
        }
        for (fx, fy, fz) in fill {
            if self.store.set_block(face, fx, fy, fz, state).is_ok() {
                updates.push((fx, fy, fz, state));
            }
        }
        for (ux, uy, uz, state_id) in updates {
            let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
                position: icbm_mc_767::packets::play::BlockPos::new(ux, uy, uz),
                state_id,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                packet.encode(w);
            })?;
        }
        Ok(())
    }

    /// 黑曜石放置后检查 4×5 传送门框架（两种朝向），构成则填充传送门方块并广播。
    fn try_build_portal(&mut self, x: i32, y: i32, z: i32) -> Result<(), RegionError> {
        use crate::portal::{PortalAxis, detect_portal_frame, portal_state};
        // 先快照检测窗口内方块（避免闭包持 &mut store 与 set_block 冲突）
        let snapshot: std::collections::HashMap<(i32, i32, i32), i32> = {
            let mut map = std::collections::HashMap::new();
            for by in (y - 4)..=(y + 4) {
                for bx in (x - 4)..=(x + 4) {
                    for bz in (z - 4)..=(z + 4) {
                        let state = self
                            .store
                            .block_at(Face::Overworld, bx, by, bz)
                            .unwrap_or(0);
                        map.insert((bx, by, bz), state);
                    }
                }
            }
            map
        };
        let lookup = |bx: i32, by: i32, bz: i32| snapshot.get(&(bx, by, bz)).copied().unwrap_or(0);
        // 放置点可能是框架任意一格（底部/顶部横梁、两侧立柱），对两种朝向分别
        // 搜索框架原点（原点 = 放置点沿厚度方向不动、沿宽度方向回退 0..3 格）。
        let found: Option<FoundFrame> = {
            let mut result = None;
            'search: for axis in [PortalAxis::X, PortalAxis::Z] {
                let (dx_offsets, dz_offsets): (Vec<i32>, Vec<i32>) = match axis {
                    PortalAxis::X => (vec![0], (-3..=0).collect()),
                    PortalAxis::Z => ((-3..=0).collect(), vec![0]),
                };
                for &dx in &dx_offsets {
                    for &dz in &dz_offsets {
                        let base_x = x + dx;
                        let base_z = z + dz;
                        for bottom in (y - 4)..=y {
                            if let Some(fill) =
                                detect_portal_frame(base_x, base_z, bottom, axis, &lookup)
                            {
                                result = Some((axis, fill));
                                break 'search;
                            }
                        }
                    }
                }
            }
            result
        };
        let Some((axis, fill)) = found else {
            return Ok(());
        };
        // 填充传送门方块
        let state = portal_state(axis);
        for (fx, fy, fz) in &fill {
            if self
                .store
                .set_block(Face::Overworld, *fx, *fy, *fz, state)
                .is_ok()
            {
                let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
                    position: icbm_mc_767::packets::play::BlockPos::new(*fx, *fy, *fz),
                    state_id: state,
                };
                self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                    packet.encode(w);
                })?;
            }
        }
        Ok(())
    }

    /// 流体计划 tick（vanilla `LevelTicks.tick` 的 fluidTicks 段 + `LiquidBlock`
    /// onPlace/neighborChanged → `scheduleTick`）。
    ///
    /// 分两阶段：先调度（放置/邻居变化的流体方块 → `scheduleTick(pos, fluid,
    /// getTickDelay)`），再执行到期 tick（vanilla `FlowingFluid.tick`：非源块
    /// getNewLiquid 更新自身 + spread 扩散）。
    fn tick_fluid_ticks(&mut self) -> Result<(), RegionError> {
        use crate::ticks::{MAX_TICKS_PER_TICK, ScheduledTick, TickPriority};
        // 1. 调度阶段：放置/移除/邻居变化的流体方块 → 计划 tick
        //    （vanilla `LiquidBlock.onPlace`/`neighborChanged` → `scheduleTick`）。
        for (x, y, z) in self.store.fluid_schedule_positions() {
            let state = self.store.block_at(Face::Overworld, x, y, z).unwrap_or(0);
            let Some(kind) = crate::fluids::FluidKind::from_state_id(state) else {
                continue; // 该位置已无流体（被移除/替换），无自身 tick 可调度
            };
            let order = self.next_sub_tick_order();
            self.fluid_ticks.schedule(ScheduledTick {
                kind,
                pos: (x, y, z),
                trigger_tick: self.world_time.max(0) as u64 + kind.tick_delay(),
                priority: TickPriority::Normal,
                sub_tick_order: order,
            });
        }
        // 2. 执行到期 tick（vanilla `LevelTicks.tick(l, 65536, this::tickFluid)`）。
        let game_time = self.world_time.max(0) as u64;
        let due = self.fluid_ticks.drain_due(game_time, MAX_TICKS_PER_TICK);
        if due.is_empty() {
            return Ok(());
        }
        let mut updates: Vec<(i32, i32, i32, i32)> = Vec::new();
        for tick in due {
            let state = self
                .store
                .block_at(Face::Overworld, tick.pos.0, tick.pos.1, tick.pos.2)
                .unwrap_or(0);
            if crate::fluids::FluidKind::from_state_id(state) != Some(tick.kind) {
                continue; // 方块已变化（vanilla `tickFluid` 校验 fluidState.is(fluid)）
            }
            let result =
                crate::fluids::tick_fluid_at(tick.pos.0, tick.pos.1, tick.pos.2, |x, y, z| {
                    self.store.block_at(Face::Overworld, x, y, z).unwrap_or(0)
                });
            for update in &result.updates[..result.len] {
                updates.push((update.x, update.y, update.z, update.state_id));
            }
            // 非源块状态变化：vanilla 重新调度自身（`scheduleTick(self, delay)`）
            if result.reschedule_self {
                let order = self.next_sub_tick_order();
                self.fluid_ticks.schedule(ScheduledTick {
                    kind: tick.kind,
                    pos: tick.pos,
                    trigger_tick: game_time + tick.kind.tick_delay(),
                    priority: TickPriority::Normal,
                    sub_tick_order: order,
                });
            }
        }
        // 3. 应用更新并广播（vanilla `spreadTo` → `setBlock`；新流体方块经
        //    `store.set_block` 回流到 fluid_schedule，下一 tick 继续调度扩散）。
        for (x, y, z, state) in updates {
            if self
                .store
                .set_block(Face::Overworld, x, y, z, state)
                .is_ok()
            {
                let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
                    position: icbm_mc_767::packets::play::BlockPos::new(x, y, z),
                    state_id: state,
                };
                self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                    packet.encode(w);
                })?;
            }
        }
        Ok(())
    }

    /// 下一个调度顺序号（vanilla `subTickOrder`：同 tick 同优先级 FIFO 决胜）。
    #[allow(clippy::missing_const_for_fn)] // 修改 self.sub_tick_order，不能 const
    fn next_sub_tick_order(&mut self) -> u64 {
        let order = self.sub_tick_order;
        self.sub_tick_order = self.sub_tick_order.wrapping_add(1);
        order
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
            player_positions: std::collections::HashMap::new(),
            player_chunks: std::collections::HashMap::new(),
            running: true,
            unknown_messages: 0,
            pending_chunks: std::collections::VecDeque::new(),
            entity_base: 0,
            entities: crate::entities::EntityManager::new(),
            mobs: crate::mobs::MobManager::new(),
            creepers: Vec::new(),
            rng: rng | 1,
            gamerules: std::collections::HashMap::new(),
            world_time: 0,
            weather: crate::weather::WeatherState::new(),
            last_time_broadcast: Instant::now(),
            command_block_fired: std::collections::HashMap::new(),
            piston_powered: std::collections::HashMap::new(),
            fluid_ticks: crate::ticks::LevelTicks::new(),
            sub_tick_order: 0,
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
        debug!(
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
                    debug!(id = %self.config.id.service_name(), "region registered");
                    return Ok(());
                }
                return Err(RegionError::RegistrationRejected);
            }
        }
        Err(RegionError::RegistrationRejected)
    }

    /// 模拟 tick（vanilla `ServerLevel.tick` 顺序：weather → tickTime →
    /// blockTicks → fluidTicks → blockEvents → entities → blockEntities；
    /// icbm 无 world border/raids/sleep，对应段省略）。
    fn tick_simulate(&mut self) -> Result<(), RegionError> {
        self.ticker.begin();
        self.drain_messages()?;
        self.flush_chunks()?;
        self.tick_weather()?;
        self.tick_time()?;
        self.tick_fluid_ticks()?;
        self.tick_redstone()?;
        self.tick_command_blocks()?;
        self.tick_pistons()?;
        self.tick_crops()?;
        self.tick_entities()?;
        self.tick_mobs()?;
        // Phase 1 世界 tick：仅随机漫游 Creeper（region 存活信号）
        self.tick_creepers()?;
        let tps = self.ticker.end();
        if self.ticker.should_heartbeat() {
            self.send_heartbeat(tps);
            self.ticker.mark_heartbeat();
        }
        self.ticker.wait_next();
        if self.players.is_empty() {
            debug!(id = %self.config.id.service_name(), "no players, sleeping");
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
            debug!(id = %self.config.id.service_name(), "player entered, waking up");
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

    #[allow(clippy::too_many_lines)] // IPC 消息分发 match 逐条对应协议
    fn handle_message(&mut self, message: &IpcMessage) -> Result<(), RegionError> {
        match message.kind {
            kind::PLAYER_ENTER => {
                let (uuid, pos, _) = decode_player_enter(&message.body)?;
                self.players.insert(uuid);
                self.player_positions.insert(uuid, (pos[0], pos[1], pos[2]));
                debug!(%uuid, "player entered region");
                // 把已存在的 Creeper 补发给新进入的玩家（客户端需 ADD_ENTITY 才渲染）
                self.resend_creepers()?;
                // 同步当前天气梯度（晴雨包只发梯度，客户端据此渲染天气）
                self.sync_weather_to_players()?;
            }
            kind::PLAYER_LEAVE => {
                let uuid = decode_player_leave(&message.body)?;
                self.players.remove(&uuid);
                self.player_positions.remove(&uuid);
                self.player_chunks.remove(&uuid);
                debug!(%uuid, "player left region");
            }
            kind::PLAYER_MOVE => {
                let (uuid, x, y, z) = decode_player_move(&message.body)?;
                self.player_positions.insert(uuid, (x, y, z));
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
            kind::GAMERULE_UPDATE => {
                if let Ok((name, value)) = decode_gamerule_update(&message.body) {
                    self.gamerules.insert(name, value);
                }
            }
            kind::TIME_UPDATE => {
                if let Ok((tick, add)) = decode_time_update(&message.body) {
                    self.world_time = if add {
                        self.world_time.wrapping_add(tick)
                    } else {
                        tick
                    };
                }
            }
            kind::WEATHER_UPDATE => {
                if let Ok((raining, thundering, rain, thunder)) =
                    decode_weather_update(&message.body)
                {
                    let toggled = self
                        .weather
                        .apply_from_core(raining, thundering, rain, thunder);
                    // 晴雨切换：广播 START/STOP（对照 vanilla `tickWeather`）
                    if toggled {
                        let event = if raining {
                            crate::weather::GAME_EVENT_RAIN_STARTED
                        } else {
                            crate::weather::GAME_EVENT_RAIN_STOPPED
                        };
                        let packet = icbm_mc_767::packets::play::clientbound::CGameEvent {
                            event,
                            value: 0.0,
                        };
                        self.send_entity_packet(play_ids::CLIENTBOUND_GAME_EVENT, |w| {
                            packet.encode(w);
                        })?;
                    }
                    // 梯度广播
                    self.sync_weather_to_players()?;
                }
            }
            kind::BLOCK_NBT_GET => {
                let (dim, x, y, z) = decode_block_nbt_get(&message.body)?;
                let nbt = self.store.block_entity(x, y, z).cloned();
                self.ipc.send(
                    CORE_SERVICE,
                    kind::BLOCK_NBT_RESP,
                    &encode_block_nbt_resp(dim, x, y, z, nbt.as_ref()),
                )?;
            }
            kind::BLOCK_NBT_SET => {
                let (dim, x, y, z, nbt) = decode_block_nbt_set(&message.body)?;
                let _ = dim;
                self.store.set_block_entity(x, y, z, nbt);
            }
            kind::BLOCK_RAYCAST => {
                let (origin, dir, max) = decode_block_raycast(&message.body)?;
                let (hit, x, y, z) = raycast_block(&mut self.store, origin, dir, max);
                self.ipc.send(
                    CORE_SERVICE,
                    kind::BLOCK_RAYCAST_RESP,
                    &encode_block_raycast_resp(hit, x, y, z),
                )?;
            }
            kind::SURFACE_QUERY => {
                let (dim, x, z) = decode_surface_query(&message.body)?;
                let _ = dim;
                let y = surface_height(&mut self.store, x, z);
                self.ipc
                    .send(CORE_SERVICE, kind::SURFACE_RESP, &encode_surface_resp(y))?;
            }
            kind::BLOCK_QUERY => {
                let (dim, x, y, z) = decode_block_query(&message.body)?;
                let Some(face) = Face::from_byte(dim) else {
                    return Err(RegionError::InvalidMessage(format!("bad dim {dim}")));
                };
                let state = self.store.block_at(face, x, y, z).unwrap_or(0);
                self.ipc.send(
                    CORE_SERVICE,
                    kind::BLOCK_QUERY_RESP,
                    &encode_block_query_resp(dim, x, y, z, state),
                )?;
            }
            kind::BLOCK_UPDATE => {
                let (dim, x, y, z, state_id) = decode_block_update(&message.body)?;
                let Some(face) = Face::from_byte(dim) else {
                    return Err(RegionError::InvalidMessage(format!("bad dim {dim}")));
                };
                self.apply_block_update(face, x, y, z, state_id)?;
            }
            kind::PORTAL_CREATE => {
                let (dim, x, y, z) = decode_portal_create(&message.body)?;
                let Some(face) = Face::from_byte(dim) else {
                    return Err(RegionError::InvalidMessage(format!("bad dim {dim}")));
                };
                self.create_portal(face, x, y, z)?;
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
    ///
    /// 顺序按**切比雪夫环从中心向外**：玩家所在区块最先送达，脚下地形在
    /// 1 tick 内出现，避免玩家在区块加载期间坠入虚空（区块请求半径 8 →
    /// 289 块，若中心块排在第 144 位、每 tick 8 块需约 0.9 秒，玩家已落到
    /// 世界底部之下）。
    ///
    /// 主世界（`Face::Overworld`）只入队本区域拥有的区块：视野跨区域时 core
    /// 向每个覆盖 region 分别请求（沉睡 region 保持在线服务内存数据；已卸载
    /// region 由 core 直读 .mca）。若本区域把邻区区块以 flat 生成重发，会覆盖
    /// 客户端已有的真实地形（“区域切换显示”BUG 根因）。地狱面仅内存 flat，
    /// flat 生成即真实地形，维持整视距发送。
    fn serve_chunks(&mut self, face: Face, cx: i32, cz: i32, radius: i32) {
        let radius = radius.clamp(0, 32);
        let region = *self.store.id();
        let owned = |chunk_x: i32, chunk_z: i32| match face {
            Face::Overworld => chunk_x >> 5 == region.rx && chunk_z >> 5 == region.rz,
            Face::Nether => true,
        };
        for ring in 0..=radius {
            if ring == 0 {
                if owned(cx, cz) {
                    self.pending_chunks.push_back((face, cx, cz));
                }
                continue;
            }
            for dx in -ring..=ring {
                for dz in -ring..=ring {
                    if (dx.abs() == ring || dz.abs() == ring) && owned(cx + dx, cz + dz) {
                        self.pending_chunks.push_back((face, cx + dx, cz + dz));
                    }
                }
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

    /// 实体 tick：掉落物物理（下落/衰减）+ TNT 引信与爆炸。
    fn tick_entities(&mut self) -> Result<(), RegionError> {
        // 掉落物：重力下落 + 随机移动，年龄增加（6000 tick 消失，vanilla 行为）
        let item_moves: Vec<(i32, i16, i16, i16)> = self
            .entities
            .items
            .iter_mut()
            .filter_map(|item| {
                item.age = item.age.saturating_add(1);
                // 1 tick 后变为可拾取
                if item.age >= 1 {
                    item.pickable = true;
                }
                // 重力：速度积累 + 位置更新
                let gravity = 0.04;
                item.vy = (item.vy - gravity).max(-0.6);
                // 简单地面碰撞：落到 superflat 地表（-61）上方时停止
                let old_x = item.x;
                let old_y = item.y;
                let old_z = item.z;
                item.x += item.vx;
                item.y += item.vy;
                item.z += item.vz;
                let surface_top = f64::from(crate::OVERWORLD_SURFACE_FEET_Y) - 1.0;
                if item.y < surface_top {
                    item.y = surface_top;
                    item.vy = 0.0;
                    item.vx = 0.0;
                    item.vz = 0.0;
                }
                let dx = ((item.x - old_x) * 4096.0) as i16;
                let dy = ((item.y - old_y) * 4096.0) as i16;
                let dz = ((item.z - old_z) * 4096.0) as i16;
                (dx != 0 || dy != 0 || dz != 0).then_some((item.entity_id, dx, dy, dz))
            })
            .collect();
        for (entity_id, dx, dy, dz) in item_moves {
            let packet = icbm_mc_767::packets::play::clientbound::CMoveEntityPos {
                entity_id,
                delta_x: dx,
                delta_y: dy,
                delta_z: dz,
                on_ground: false,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_MOVE_ENTITY_POS, |w| {
                packet.encode(w);
            })?;
        }
        // TNT：引信倒计时，归零时爆炸
        let exploded: Vec<(f64, f64, f64)> = self
            .entities
            .tnts
            .iter_mut()
            .filter_map(|tnt| {
                tnt.fuse = tnt.fuse.saturating_sub(1);
                (tnt.fuse == 0).then_some((tnt.x, tnt.y, tnt.z))
            })
            .collect();
        for (x, y, z) in exploded {
            self.detonate_tnt(x, y, z)?;
        }
        // 清理过期掉落物
        self.entities.items.retain(|item| item.age < 6000);
        // 清理已爆炸 TNT
        self.entities.tnts.retain(|tnt| tnt.fuse > 0);
        Ok(())
    }

    /// TNT 爆炸：破坏范围内方块、广播 `CExplosion` 与 `CBlockUpdate`、生成掉落物。
    fn detonate_tnt(&mut self, x: f64, y: f64, z: f64) -> Result<(), RegionError> {
        self.detonate_explosion(x, y, z, 4.0)
    }

    /// 通用爆炸：破坏范围内方块、广播 `CExplosion` 与 `CBlockUpdate`、生成掉落物。
    /// `radius`：TNT 4、Creeper 3（vanilla `TntEntity`/`CreeperEntity` 爆炸半径）。
    fn detonate_explosion(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        radius: f64,
    ) -> Result<(), RegionError> {
        use icbm_mc_767::packets::play::clientbound::{
            CBlockUpdate, CExplosion, ExplosionDestructionType,
        };
        // 计算受影响方块
        let affected = {
            let store = &mut self.store;
            let mut lookup = |bx: i32, by: i32, bz: i32| {
                store.block_at(Face::Overworld, bx, by, bz).unwrap_or(0)
            };
            crate::entities::compute_explosion(x, y, z, radius, &mut lookup).affected_blocks
        };
        // 广播 CExplosion（受影响方块 → 客户端渲染爆炸效果）
        let explosion = CExplosion {
            x,
            y,
            z,
            radius: radius as f32,
            affected_blocks: affected
                .iter()
                .map(|&(bx, by, bz)| icbm_mc_767::packets::play::BlockPos::new(bx, by, bz))
                .collect(),
            player_velocity: [0.0, 0.0, 0.0],
            destruction_type: ExplosionDestructionType::Destroy,
            particle: "large_smoke".to_owned(),
            emitter_particle: "explosion_emitter".to_owned(),
            sound: "entity.generic.explode".to_owned(),
        };
        self.send_entity_packet(play_ids::CLIENTBOUND_EXPLOSION, |w| {
            explosion.encode(w);
        })?;
        // 破坏方块并广播更新、掉落物
        let mut spawns: Vec<(f64, f64, f64, String, i32)> = Vec::new();
        for (bx, by, bz) in &affected {
            let state = self
                .store
                .block_at(Face::Overworld, *bx, *by, *bz)
                .unwrap_or(0);
            if let Some(name) = icbm_mc_767::registry::state_id_to_name(state) {
                let item_name = name.strip_prefix("minecraft:").unwrap_or(name);
                spawns.push((
                    f64::from(*bx),
                    f64::from(*by),
                    f64::from(*bz),
                    item_name.to_owned(),
                    1,
                ));
            }
            self.store.set_block(Face::Overworld, *bx, *by, *bz, 0)?;
            let packet = CBlockUpdate {
                position: icbm_mc_767::packets::play::BlockPos::new(*bx, *by, *bz),
                state_id: 0,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                packet.encode(w);
            })?;
        }
        // 生成掉落物（方块破坏掉落）
        for (sx, sy, sz, item, count) in spawns {
            let entity_id = self.entity_base
                + 1000
                + i32::try_from(self.entities.items.len()).unwrap_or(i32::MAX);
            self.entities
                .spawn_item(entity_id, sx, sy + 0.5, sz, &item, count);
            let item_entity = self.entities.items.last().expect("just spawned").clone();
            let add = icbm_mc_767::packets::play::clientbound::CAddEntity {
                entity_id: item_entity.entity_id,
                uuid: item_entity.uuid,
                entity_type: crate::entities::entity_type_ids::ITEM,
                x: item_entity.x,
                y: item_entity.y,
                z: item_entity.z,
                pitch: 0.0,
                yaw: 0.0,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_ADD_ENTITY, |w| {
                add.encode(w);
            })?;
        }
        Ok(())
    }

    /// 红石 tick：对脏区块内的红石线计算供电（6 方向电源 + 垂直/对角传导）、
    /// 红石灯根据相邻信号点亮/熄灭。
    fn tick_redstone(&mut self) -> Result<(), RegionError> {
        use crate::redstone::{compute_wire_power, neighbor_has_signal, wire_power, wire_state};
        use icbm_mc_767::packets::play::block_state_ids as ids;
        let chunks = self.store.redstone_chunks();
        if chunks.is_empty() {
            return Ok(());
        }
        // 1. 先收集所有红石线位置与当前强度（快照，避免 tick 中反复查询）
        let mut wires: Vec<(i32, i32, i32)> = Vec::new();
        let mut lamps: Vec<(i32, i32, i32)> = Vec::new();
        let mut snapshot: std::collections::HashMap<(i32, i32, i32), i32> =
            std::collections::HashMap::new();
        for (cx, cz) in &chunks {
            for x in cx * 16..cx * 16 + 16 {
                for z in cz * 16..cz * 16 + 16 {
                    for y in -64..319 {
                        let state = self.store.block_at(Face::Overworld, x, y, z).unwrap_or(0);
                        snapshot.insert((x, y, z), state);
                        if wire_power(state).is_some() {
                            wires.push((x, y, z));
                        } else if state == ids::REDSTONE_LAMP || state == ids::REDSTONE_LAMP_ON {
                            lamps.push((x, y, z));
                        }
                    }
                }
            }
        }
        // 2. 计算每条红石线的信号强度（vanilla `calculateTargetStrength`：
        //    6 方向电源/强信号穿透 + 线传导衰减）
        let mut wire_updates: Vec<(i32, i32, i32, i32)> = Vec::new();
        for (x, y, z) in wires {
            let power = compute_wire_power(&snapshot, x, y, z);
            let current = snapshot.get(&(x, y, z)).copied().unwrap_or(0);
            if wire_power(current) != Some(power) {
                wire_updates.push((x, y, z, wire_state(power)));
            }
        }
        // 3. 红石灯：相邻任一信号 > 0 点亮（`hasNeighborSignal`；线按全额强度）
        let mut lamp_updates: Vec<(i32, i32, i32, i32)> = Vec::new();
        for (x, y, z) in lamps {
            let lit = neighbor_has_signal(&snapshot, x, y, z);
            let target = crate::redstone::lamp_state(lit);
            let current = snapshot.get(&(x, y, z)).copied().unwrap_or(0);
            if current != target {
                lamp_updates.push((x, y, z, target));
            }
        }
        // 4. 应用更新并广播。若本 tick 有线/灯状态变化，重新标记区块脏，
        // 让功率沿导线逐 tick 传播（对照 vanilla 红石 1 tick/格的传播速率）；
        // 若仍持续变化，下一 tick 继续处理直到稳定。
        let mut any_changed = false;
        for (x, y, z, state) in wire_updates.into_iter().chain(lamp_updates) {
            if self
                .store
                .set_block(Face::Overworld, x, y, z, state)
                .is_ok()
            {
                any_changed = true;
                let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
                    position: icbm_mc_767::packets::play::BlockPos::new(x, y, z),
                    state_id: state,
                };
                self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                    packet.encode(w);
                })?;
            }
        }
        if any_changed {
            for (cx, cz) in chunks {
                self.store.mark_redstone_region(cx, cz);
            }
        }
        Ok(())
    }

    /// 读取布尔游戏规则（默认 `true`）。
    fn gamerule_bool(&self, name: &str) -> bool {
        self.gamerules.get(name).is_none_or(|value| value == "true")
    }

    /// 世界时间 tick：`doDaylightCycle` 控制推进，广播 `CUpdateTime`。
    fn tick_time(&mut self) -> Result<(), RegionError> {
        // 世界时间由 core 全局管理（`TIME_UPDATE` 每秒同步），region 不再
        // 自行推进，避免跨区域漂移。这里只负责广播 `CUpdateTime`。
        let daylight = self.gamerule_bool("doDaylightCycle");
        // 约每秒广播一次（节流，避免每 tick 刷包）
        if self.last_time_broadcast.elapsed() >= Duration::from_secs(1) {
            let packet = icbm_mc_767::packets::play::clientbound::CUpdateTime {
                game_time: self.world_time,
                day_time: if daylight {
                    self.world_time
                } else {
                    // 冻结：负数表示停止昼夜循环（对照 `WorldTimeUpdateS2CPacket`）
                    -self.world_time
                },
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_SET_TIME, |w| {
                packet.encode(w);
            })?;
            self.last_time_broadcast = Instant::now();
        }
        Ok(())
    }

    /// 命令方块 tick：被红石信号供电（相邻方块任一信号 > 0，vanilla
    /// `hasNeighborSignal`——红石线按全额强度计）且尚未触发时，经
    /// `COMMAND_EXECUTE` 交给 core 以控制台身份执行其 `Command` NBT。
    ///
    /// 三种命令方块（普通/循环/连锁）统一按“上升沿触发一次”处理（简化）；
    /// 循环方块原版每 tick 执行，此处同样只触发一次（避免刷屏）。
    /// 位置表由 store 维护（`set_block` 增量更新），避免全量扫描。
    fn tick_command_blocks(&mut self) -> Result<(), RegionError> {
        use crate::redstone::neighbor_has_signal;
        let positions = self.store.command_block_positions();
        if positions.is_empty() {
            return Ok(());
        }
        // 供电检测快照（命令方块 ±1 范围）
        let mut snapshot: std::collections::HashMap<(i32, i32, i32), i32> =
            std::collections::HashMap::new();
        for &(x, y, z) in &positions {
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let key = (x + dx, y + dy, z + dz);
                        snapshot.entry(key).or_insert_with(|| {
                            self.store
                                .block_at(Face::Overworld, key.0, key.1, key.2)
                                .unwrap_or(0)
                        });
                    }
                }
            }
        }
        let mut executions: Vec<(i32, i32, i32, String)> = Vec::new();
        for (x, y, z) in positions {
            let powered = neighbor_has_signal(&snapshot, x, y, z);
            let fired = self
                .command_block_fired
                .get(&(x, y, z))
                .copied()
                .unwrap_or(false);
            if powered && !fired {
                if let Some(command) = self.store.command_block_command(x, y, z) {
                    executions.push((x, y, z, command));
                }
                self.command_block_fired.insert((x, y, z), true);
            } else if !powered {
                self.command_block_fired.insert((x, y, z), false);
            }
        }
        for (x, y, z, command) in executions {
            debug!(x, y, z, %command, "command block executed");
            self.ipc.send_retry(
                CORE_SERVICE,
                kind::COMMAND_EXECUTE,
                &encode_command_execute(&command),
            )?;
        }
        Ok(())
    }

    /// 活塞 tick：BUD（QC 供电）与伸出/缩回，推动方块并广播 `CBlockUpdate`。
    fn tick_pistons(&mut self) -> Result<(), RegionError> {
        let positions = self.store.piston_positions();
        if positions.is_empty() {
            return Ok(());
        }
        let actions = crate::pistons::tick_pistons(
            &positions,
            &mut self.piston_powered,
            &mut PistonStoreAccess(&mut self.store),
        );
        for action in actions {
            for ((x, y, z), state) in action.updates {
                let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
                    position: icbm_mc_767::packets::play::BlockPos::new(x, y, z),
                    state_id: state,
                };
                self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                    packet.encode(w);
                })?;
            }
        }
        Ok(())
    }

    /// 农作物随机 tick 生长：每个已加载区块按 `randomTickSpeed`（默认 3）次
    /// 抽查地表顶方块，命中作物且未成熟时以 1/3 概率推进 age（广播 `CBlockUpdate`）。
    fn tick_crops(&mut self) -> Result<(), RegionError> {
        let random_ticks = self
            .gamerules
            .get("randomTickSpeed")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3)
            .clamp(0, 1000);
        if random_ticks == 0 {
            return Ok(());
        }
        // 只处理存在作物的位置（set_block 维护；避免每 tick 全量 heightmap 扫描
        // ——289 区块 × 384×256 计算会让 region tick 数百毫秒、延迟 IPC 响应）
        let positions = self.store.crop_positions();
        if positions.is_empty() {
            return Ok(());
        }
        let mut growth: Vec<(i32, i32, i32, i32)> = Vec::new();
        for &(x, y, z) in &positions {
            // 每 tick 以 randomTickSpeed 概率抽查（vanilla 随机 tick 近似）
            if self.rand_f64() >= 1.0 / random_ticks.max(1) as f64 {
                continue;
            }
            let state = self.store.block_at(Face::Overworld, x, y, z).unwrap_or(0);
            let Some(crop) = crate::crops::is_crop(state) else {
                continue;
            };
            let age = state - crop.base;
            if age >= crop.max_age {
                continue; // 已成熟
            }
            // 生长概率：可可 2/3，其余 1/3（对照 `CropBlock`/`CocoaBlock`）
            let chance = if crop.base == icbm_mc_767::packets::play::block_state_ids::COCOA_AGE_0 {
                2.0 / 3.0
            } else {
                1.0 / 3.0
            };
            if self.rand_f64() < chance {
                growth.push((x, y, z, crate::crops::crop_state(crop, age + 1)));
            }
        }
        for (x, y, z, state) in growth {
            self.store.set_block(Face::Overworld, x, y, z, state)?;
            let packet = icbm_mc_767::packets::play::clientbound::CBlockUpdate {
                position: icbm_mc_767::packets::play::BlockPos::new(x, y, z),
                state_id: state,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_BLOCK_UPDATE, |w| {
                packet.encode(w);
            })?;
        }
        Ok(())
    }

    /// 天气 tick：晴雨/雷暴切换与梯度插值，广播 `GAME_EVENT`。
    fn tick_weather(&self) -> Result<(), RegionError> {
        // 天气由 core 全局管理（`WEATHER_UPDATE` 同步），region 不再自行推进。
        // 这里只把当前梯度广播给玩家（客户端据此渲染天气）。
        self.sync_weather_to_players()
    }

    /// 玩家进入后同步当前天气梯度（对照 `PlayerManager.onPlayerConnect` 的
    /// `RAIN_GRADIENT_CHANGED`/`THUNDER_GRADIENT_CHANGED`）。
    fn sync_weather_to_players(&self) -> Result<(), RegionError> {
        let rain = self.weather.rain_gradient();
        let thunder = self.weather.thunder_gradient();
        for (event, value) in [
            (crate::weather::GAME_EVENT_RAIN_GRADIENT, rain),
            (crate::weather::GAME_EVENT_THUNDER_GRADIENT, thunder),
        ] {
            let packet = icbm_mc_767::packets::play::clientbound::CGameEvent { event, value };
            self.send_entity_packet(play_ids::CLIENTBOUND_GAME_EVENT, |w| {
                packet.encode(w);
            })?;
        }
        Ok(())
    }

    /// 友好生物 tick：生成（未达上限时）、随机游走、环境音效广播。
    fn tick_mobs(&mut self) -> Result<(), RegionError> {
        let min_x = f64::from(self.config.id.rx) * 512.0;
        let min_z = f64::from(self.config.id.rz) * 512.0;
        let max_x = min_x + 512.0;
        let max_z = min_z + 512.0;
        // 生成：约每 2 秒尝试一只（上限由 MobManager 控制；`doMobSpawning` 关闭时禁止）
        if self.gamerule_bool("doMobSpawning") && self.rand_f64() < 1.0 / 40.0 {
            let entity_id =
                self.entity_base + 2000 + i32::try_from(self.mobs.len()).unwrap_or(i32::MAX);
            if self.mobs.try_spawn(entity_id, min_x, max_x, min_z, max_z) {
                let mob = self.mobs.mobs.last().expect("just spawned").clone();
                let add = icbm_mc_767::packets::play::clientbound::CAddEntity {
                    entity_id: mob.entity_id,
                    uuid: mob.uuid,
                    entity_type: mob.kind.entity_type_id(),
                    x: mob.x,
                    y: mob.ground_y(),
                    z: mob.z,
                    pitch: 0.0,
                    yaw: mob.yaw,
                };
                self.send_entity_packet(play_ids::CLIENTBOUND_ADD_ENTITY, |w| {
                    add.encode(w);
                })?;
            }
        }
        // 游走 + 环境音效
        let (moves, sounds) = self.mobs.tick(min_x, max_x, min_z, max_z);
        for (entity_id, dx, dz) in moves {
            let packet = icbm_mc_767::packets::play::clientbound::CMoveEntityPos {
                entity_id,
                delta_x: dx,
                delta_y: 0,
                delta_z: dz,
                on_ground: true,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_MOVE_ENTITY_POS, |w| {
                packet.encode(w);
            })?;
        }
        for (entity_id, kind) in sounds {
            let sound = icbm_mc_767::packets::play::clientbound::CEntitySound {
                sound: kind.ambient_sound().to_owned(),
                category: icbm_mc_767::packets::play::clientbound::SoundCategory::Neutral,
                entity_id,
                volume: 1.0,
                pitch: 1.0,
                seed: self.next_random() as i64,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_SOUND_ENTITY, |w| {
                sound.encode(w);
            })?;
        }
        Ok(())
    }

    /// 敌对生物（Creeper）tick：vanilla 生成机制 + 追逐/引爆 AI。
    ///
    /// 生成：仅在夜晚（`world_time % 24000 ∈ [13000, 23000)`）且 `doMobSpawning`
    /// 开启时按概率生成，上限 `MAX_CREEPERS`。AI：寻找最近玩家（16 格内），
    /// 向其靠近；3 格内点燃引信（广播实体状态闪烁），30 tick 后爆炸。
    #[allow(clippy::too_many_lines)] // 生成 + 追逐 + 引信 + 爆炸的事件流水
    fn tick_creepers(&mut self) -> Result<(), RegionError> {
        let night = {
            let phase = self.world_time.rem_euclid(24_000);
            (NIGHT_START..NIGHT_END).contains(&phase)
        };
        if self.gamerule_bool("doMobSpawning")
            && night
            && self.creepers.len() < MAX_CREEPERS
            && self.rand_f64() < SPAWN_CHANCE
        {
            self.spawn_creeper()?;
        }
        let min_x = f64::from(self.config.id.rx) * 512.0;
        let min_z = f64::from(self.config.id.rz) * 512.0;

        // 最近玩家位置（只取区域内玩家；core 经 PLAYER_MOVE 同步）
        let nearest: Option<(f64, f64, f64)> = {
            let mut best: Option<(f64, (f64, f64, f64))> = None;
            for (uuid, pos) in &self.player_positions {
                if !self.players.contains(uuid) {
                    continue;
                }
                let dist = (pos.0 - min_x - 256.0).powi(2) + (pos.2 - min_z - 256.0).powi(2);
                if best.as_ref().is_none_or(|(d, _)| dist < *d) {
                    best = Some((dist, *pos));
                }
            }
            best.map(|(_, pos)| pos)
        };

        let mut moves: Vec<(i32, i16, i16)> = Vec::new();
        let mut fuse_starts: Vec<i32> = Vec::new();
        let mut explosions: Vec<(i32, f64, f64, f64)> = Vec::new();
        let mut alive: Vec<Creeper> = Vec::with_capacity(self.creepers.len());
        for mut creeper in std::mem::take(&mut self.creepers) {
            if creeper.fuse >= 0 {
                // 引信倒计时
                creeper.fuse -= 1;
                if creeper.fuse <= 0 {
                    explosions.push((creeper.entity_id, creeper.x, creeper.y, creeper.z));
                    continue;
                }
                alive.push(creeper);
                continue;
            }
            // 追逐最近玩家
            if let Some((px, py, pz)) = nearest {
                let dx = px - creeper.x;
                let dz = pz - creeper.z;
                let horizontal = dx.hypot(dz);
                if horizontal <= CREEPER_FUSE_RANGE {
                    creeper.fuse = CREEPER_FUSE_TICKS;
                    fuse_starts.push(creeper.entity_id);
                    alive.push(creeper);
                    continue;
                }
                if horizontal <= CREEPER_TARGET_RANGE
                    && (py - creeper.y).abs() < 8.0
                    && self.rand_f64() < 0.2
                {
                    // 朝玩家移动 1 格（vanilla 步行速度约 0.25 格/tick）
                    let step = if dx.abs() > dz.abs() {
                        (dx.signum(), 0.0)
                    } else {
                        (0.0, dz.signum())
                    };
                    let next_x = creeper.x + step.0;
                    let next_z = creeper.z + step.1;
                    if (min_x..min_x + 512.0).contains(&next_x)
                        && (min_z..min_z + 512.0).contains(&next_z)
                    {
                        creeper.x = next_x;
                        creeper.z = next_z;
                        moves.push((
                            creeper.entity_id,
                            (step.0 * 4096.0) as i16,
                            (step.1 * 4096.0) as i16,
                        ));
                    }
                }
            }
            alive.push(creeper);
        }
        self.creepers = alive;

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
        for entity_id in fuse_starts {
            // 实体状态 1 = 苦力怕点燃引信（客户端渲染闪烁）
            let status = icbm_mc_767::packets::play::clientbound::CEntityStatus {
                entity_id,
                status: 1,
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_ENTITY_EVENT, |w| {
                status.encode(w);
            })?;
        }
        for (entity_id, x, y, z) in explosions {
            // 爆炸后移除 Creeper 实体并破坏方块
            let remove = icbm_mc_767::packets::play::clientbound::CRemoveEntities {
                entity_ids: vec![entity_id],
            };
            self.send_entity_packet(play_ids::CLIENTBOUND_REMOVE_ENTITIES, |w| {
                remove.encode(w);
            })?;
            self.detonate_explosion(x, y, z, 3.0)?;
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
            y: CREEPER_SPAWN_Y,
            z: f64::from(cz * 16) + self.rand_f64() * 16.0,
            yaw: (self.rand_f64() * 360.0) as f32,
            last_dir: 4, // 初始无方向
            fuse: -1,    // 未点燃
        };
        self.send_entity_packet(play_ids::CLIENTBOUND_ADD_ENTITY, |w| {
            CAddEntity {
                entity_id: creeper.entity_id,
                uuid: creeper.uuid,
                entity_type: ENTITY_TYPE_CREEPER,
                x: creeper.x,
                y: creeper.y,
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
                y: creeper.y,
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

/// 主世界最低 Y（`WorldHeight.min_y` 一致）。
const OVERWORLD_MIN_Y: i32 = -64;

/// 沿射线 DDA 步进找第一个非空气方块；返回命中方块坐标（未命中返回 false）。
///
/// 供末影珍珠落点计算（core 发起 `BLOCK_RAYCAST`）。只在整数格变化时查询，
/// 最多 `max` 格；方向已归一化。
fn raycast_block(
    store: &mut ChunkStore,
    origin: [f64; 3],
    dir: [f64; 3],
    max: f64,
) -> (bool, f64, f64, f64) {
    let mut x = origin[0];
    let mut y = origin[1];
    let mut z = origin[2];
    let mut last_bx = x.floor() as i32;
    let mut last_by = y.floor() as i32;
    let mut last_bz = z.floor() as i32;
    let mut distance = 0.0;
    while distance < max {
        distance += 0.1;
        x += dir[0] * 0.1;
        y += dir[1] * 0.1;
        z += dir[2] * 0.1;
        let bx = x.floor() as i32;
        let by = y.floor() as i32;
        let bz = z.floor() as i32;
        if bx == last_bx && by == last_by && bz == last_bz {
            continue;
        }
        last_bx = bx;
        last_by = by;
        last_bz = bz;
        if by < OVERWORLD_MIN_Y {
            continue;
        }
        let state = store.block_at(Face::Overworld, bx, by, bz).unwrap_or(0);
        if state != 0 {
            return (true, f64::from(bx), f64::from(by), f64::from(bz));
        }
    }
    (false, x, y, z)
}

/// 查询世界坐标 (x, z) 列的地表高度（最高非空气方块 y + 1；整列为空时 = 世界最低 Y）。
/// 供 core 出生点适配（新玩家出生在真实地表上）。
fn surface_height(store: &mut ChunkStore, x: i32, z: i32) -> i32 {
    let cx = x.div_euclid(16);
    let cz = z.div_euclid(16);
    let Ok(column) = store.get_chunk(Face::Overworld, cx, cz) else {
        return -64;
    };
    let local_x = (x & 0xF) as u32;
    let local_z = (z & 0xF) as u32;
    column.heightmap_heights()[(local_z * 16 + local_x) as usize]
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

#[cfg(test)]
mod raycast_tests {
    use super::raycast_block;
    use crate::store::ChunkStore;
    use crate::{Face, RegionId};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("raycast-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn raycast_hits_pillar() {
        let dir = temp_dir("pillar");
        let mut store = ChunkStore::open(&dir, RegionId { rx: 0, rz: 0 }).unwrap();
        for y in -62..=-58 {
            store.set_block(Face::Overworld, 10, y, 0, 1).unwrap(); // stone
        }
        // 玩家眼睛 (0,-58.38,0) 面向 +x
        let (hit, x, y, z) = raycast_block(&mut store, [0.0, -58.38, 0.0], [1.0, 0.0, 0.0], 120.0);
        assert!(hit, "ray should hit the pillar");
        assert_eq!((x as i32, y as i32, z as i32), (10, -59, 0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn raycast_misses_empty_space() {
        let dir = temp_dir("empty");
        let mut store = ChunkStore::open(&dir, RegionId { rx: 0, rz: 0 }).unwrap();
        let (hit, _, _, _) = raycast_block(&mut store, [0.0, -58.38, 0.0], [1.0, 0.0, 0.0], 50.0);
        assert!(!hit);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// 活塞方块访问适配器（`ChunkStore` → `BlockAccess`）。
struct PistonStoreAccess<'a>(&'a mut ChunkStore);

impl crate::pistons::BlockAccess for PistonStoreAccess<'_> {
    fn block_at(&mut self, x: i32, y: i32, z: i32) -> i32 {
        self.0.block_at(Face::Overworld, x, y, z).unwrap_or(0)
    }

    fn set_block(&mut self, x: i32, y: i32, z: i32, state: i32) {
        let _ = self.0.set_block(Face::Overworld, x, y, z, state);
    }
}
