//! 服务器主结构：配置、datarouter、世界状态、接收/控制台线程与 accept 循环。

use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use blinkcms_datarouter::switch_frontend::{SwitchConfig, SwitchError, SwitchHandle};
use icbm_anvil::level::{LevelData, write_level_dat};
use icbm_mc_767::packets::play::clientbound::{
    CChunkBatchFinished, CChunkBatchStart, CRemoveEntities,
};
use icbm_mc_767::packets::play_ids;
use icbm_region::RegionId;
use icbm_region::protocol::{
    DIM_OVERWORLD, Ipc, IpcError, decode_block_nbt_resp, decode_block_query_resp,
    decode_block_raycast_resp, decode_chunk_data, decode_command_execute, decode_entity_data,
    decode_heartbeat, decode_register, decode_surface_resp, decode_worker_register,
    encode_register_ack, encode_unload, encode_worker_region, kind,
};
use tracing::{debug, info, trace, warn};

use crate::client::{Client, NetError};
use crate::config::ServerConfig;
use crate::region_registry::warn_timed_out;
use crate::world::WorldState;

/// 服务器错误。
#[derive(Debug)]
pub enum ServerError {
    /// IO 错误。
    Io(std::io::Error),
    /// datarouter IPC 错误。
    Ipc(IpcError),
    /// switch 拉起/鉴权失败。
    Switch(SwitchError),
    /// level.dat 错误。
    Level(icbm_anvil::level::LevelError),
    /// 登录加密密钥生成失败。
    Crypto(crate::net::crypto::CryptoError),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io: {err}"),
            Self::Ipc(err) => write!(f, "ipc: {err}"),
            Self::Switch(err) => write!(f, "switch: {err}"),
            Self::Level(err) => write!(f, "level: {err}"),
            Self::Crypto(err) => write!(f, "crypto: {err}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<std::io::Error> for ServerError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<IpcError> for ServerError {
    fn from(value: IpcError) -> Self {
        Self::Ipc(value)
    }
}

impl From<SwitchError> for ServerError {
    fn from(value: SwitchError) -> Self {
        Self::Switch(value)
    }
}

impl From<icbm_anvil::level::LevelError> for ServerError {
    fn from(value: icbm_anvil::level::LevelError) -> Self {
        Self::Level(value)
    }
}

/// 服务器内部状态（Arc 共享给各线程）。
pub struct ServerInner {
    /// 配置。
    pub config: ServerConfig,
    /// core 的 datarouter 前端。
    pub ipc: Ipc,
    /// 嵌入式 `datarouter-switch`（core 拉起并持有；外部 switch 时为空）。
    /// 仅靠 Drop 终止 switch 进程，故无读取点，标记为 allowed。
    #[allow(dead_code)]
    switch: Option<SwitchHandle>,
    /// 世界状态。
    pub world: Mutex<WorldState>,
    /// 运行标志。
    pub running: AtomicBool,
    /// 正版模式开关（`ICBM_ONLINE_MODE`）。
    pub online_mode: crate::net::crypto::OnlineMode,
    /// 登录 RSA 密钥对（正版/加密登录用）。
    pub key_store: crate::net::crypto::KeyStore,
    /// 白名单与封禁列表。
    pub lists: std::sync::Mutex<crate::lists::LoginLists>,
    /// 收到的 `CHUNK_DATA` 计数（诊断）。
    pub chunk_diag: std::sync::atomic::AtomicU64,
    /// 全局世界时间（tick；core 管理，广播给所有 region，避免各 region 漂移）。
    pub world_time: Mutex<i64>,
    /// 全局天气（core 管理，广播给所有 region）。
    pub weather: Mutex<icbm_gameplay_minecraft::weather::WeatherState>,
}

/// 服务器（可克隆共享）。
#[derive(Clone)]
pub struct Server {
    inner: Arc<ServerInner>,
}

impl Server {
    /// 初始化：世界目录、level.dat、datarouter 连接。
    pub fn new(config: ServerConfig) -> Result<Self, ServerError> {
        std::fs::create_dir_all(&config.world_dir)?;
        let level_path = config.world_dir.join("level.dat");
        if !level_path.exists() {
            write_level_dat(
                &level_path,
                &LevelData {
                    level_name: "world".to_owned(),
                    gamerules: Vec::new(),
                    player: None,
                },
            )?;
            info!("created level.dat");
        }
        let auth_token = std::env::var("ICBM_DATAROUTER_TOKEN")
            .ok()
            .filter(|token| !token.is_empty());
        let switch = start_switch(&config, auth_token.clone())?;
        let ipc = Ipc::connect(&config.shm_name, "icbm.core", auth_token)?;
        info!("connected to datarouter {}", config.shm_name);
        // worker 进程池：`cpu_count - 2` 个常驻 worker（core 除外），平均绑核
        // （`build/workspace/draft/worker_pool/README.md`）。
        spawn_worker_pool(&config);
        let world = Mutex::new(WorldState::new(config.world_dir.clone()));
        let world_dir_for_lists = config.world_dir.clone();
        // 从 level.dat 恢复游戏规则（`GameRules` 复合，对照 `LevelSave` 启动路径）
        if let Ok(level) = icbm_anvil::level::read_level_dat(&level_path) {
            let mut state = world.lock().expect("world lock");
            for (name, value) in &level.gamerules {
                if let Err(err) = state.gamerules.set(name, value) {
                    warn!(%err, rule = %name, "ignoring stale gamerule from level.dat");
                }
            }
        }
        Ok(Self {
            inner: Arc::new(ServerInner {
                config,
                ipc,
                switch,
                world,
                running: AtomicBool::new(true),
                online_mode: crate::net::crypto::online_mode_from_env(),
                key_store: crate::net::crypto::KeyStore::new().map_err(ServerError::Crypto)?,
                lists: std::sync::Mutex::new(crate::lists::LoginLists::load(world_dir_for_lists)),
                chunk_diag: std::sync::atomic::AtomicU64::new(0),
                world_time: Mutex::new(0),
                weather: Mutex::new(icbm_gameplay_minecraft::weather::WeatherState::new()),
            }),
        })
    }

    /// 把当前游戏规则广播给所有 region（`GAMERULE_UPDATE`）。
    ///
    /// 规则变更后调用；region 据此控制昼夜循环、生物生成等行为。
    pub fn broadcast_gamerules(&self) {
        let rules: Vec<(String, String)> = {
            let world = self.world.lock().expect("world lock");
            world
                .gamerules
                .iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect()
        };
        let regions: Vec<RegionId> = {
            let world = self.world.lock().expect("world lock");
            world.regions.ids()
        };
        for region in regions {
            for (name, value) in &rules {
                let body = icbm_region::protocol::encode_gamerule_update(name, value);
                let _ = self.ipc.send(
                    &region.service_name(),
                    icbm_region::protocol::kind::GAMERULE_UPDATE,
                    &body,
                );
            }
        }
    }

    /// 把当前游戏规则持久化到 level.dat（`GameRules` 复合）。
    pub fn persist_gamerules(&self) {
        let path = self.config.world_dir.join("level.dat");
        let mut level = icbm_anvil::level::read_level_dat(&path).unwrap_or(LevelData {
            level_name: "world".to_owned(),
            gamerules: Vec::new(),
            player: None,
        });
        let gamerules: Vec<(String, String)> = {
            let world = self.world.lock().expect("world lock");
            world
                .gamerules
                .iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect()
        };
        level.gamerules = gamerules;
        if let Err(err) = icbm_anvil::level::write_level_dat(&path, &level) {
            warn!(%err, "failed to persist gamerules");
        }
    }

    /// 运行主循环直至停止。
    pub fn run(self) -> Result<(), ServerError> {
        spawn_receiver(&self);
        crate::console::spawn_console(&self);

        let listener = TcpListener::bind(&self.config.address)?;
        // 非阻塞 accept：running=false 时轮询退出（避免阻塞卡死 stop）
        listener.set_nonblocking(true).map_err(ServerError::Io)?;
        info!("listening on {}", self.config.address);

        let mut handles = Vec::new();
        while self.running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    let server = self.clone();
                    handles.push(std::thread::spawn(move || {
                        crate::client::handle_connection(&server, stream);
                    }));
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    // 无新连接：短暂等待后继续轮询 running
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    warn!(%err, "accept failed");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }

        // 停止：断开玩家、退出 region（保存存档）、等待连接线程
        let mut world = self.world.lock().expect("world lock");
        for client in world.players.values() {
            client.alive.store(false, Ordering::Relaxed);
            client.kick("服务器关闭");
        }
        world.players.clear();
        drop(world);
        // 连接线程在 read timeout（100ms）内观察到 running=false 后退出
        for handle in handles {
            handle.join().ok();
        }
        // 保存并卸载所有 region（发送 UNLOAD，region flush 后退出；最多等 5 秒）
        self.shutdown_regions();
        Ok(())
    }

    /// 保存并卸载所有 region：逐个发 `REGION_UNLOAD`（region 线程 flush 后退出；
    /// worker 进程是容器，region 全部退出后由 kill 清理）。
    fn shutdown_regions(&self) {
        let regions: Vec<RegionId> = {
            let world = self.world.lock().expect("world lock");
            world.regions.ids()
        };
        for id in &regions {
            info!(id = %id.service_name(), "shutting down region");
            let _ = self.ipc.send(
                &id.service_name(),
                kind::REGION_UNLOAD,
                &encode_unload(DIM_OVERWORLD, id.rx, id.rz),
            );
        }
        // 等待 region 完成 flush 并退出（region 收到 UNLOAD 后 flush → running=false）
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let remaining = self.world.lock().expect("world lock").regions.len();
            if remaining == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // 清理 worker 进程池（region 已退出；worker 是常驻容器，关停时 kill）
        let worker_pids: Vec<u32> = {
            let world = self.world.lock().expect("world lock");
            world
                .workers
                .ids()
                .into_iter()
                .filter_map(|id| world.workers.get(id).map(|entry| entry.pid))
                .collect()
        };
        for pid in worker_pids {
            debug!(pid, "stopping worker process");
            let _ = kill_process(pid);
        }
        self.world.lock().expect("world lock").regions.clear_all();
    }

    /// 确保 region 已分配到 worker 并注册（无则等待最多 10 秒）。
    ///
    /// 分配：选负载最低的 worker 发 `WORKER_ASSIGN`（region 合并）；worker
    /// 宿主内启动 region 模拟线程（注册 `icbm.region.<id>`）。不再每 region
    /// spawn 一个进程，卸载也不杀进程。
    pub fn ensure_region(&self, id: RegionId) -> Result<(), NetError> {
        if self
            .world
            .lock()
            .expect("world lock")
            .regions
            .get(&id)
            .is_some_and(|entry| entry.alive)
        {
            return Ok(());
        }
        self.assign_region_if_needed(id)?;
        self.wait_region_registered(id)
    }

    /// 把 region 分配出去（worker 或直接 spawn），不等待注册。
    ///
    /// 预加载（`preload_neighbor_regions`）用：region 已在 worker 内注册中时
    /// 幂等返回；无 worker 回退直接 spawn（兼容）。
    pub fn assign_region_if_needed(&self, id: RegionId) -> Result<(), NetError> {
        // 已注册或已分配（注册中）：直接返回
        if self
            .world
            .lock()
            .expect("world lock")
            .regions
            .get(&id)
            .is_some_and(|entry| entry.alive || entry.worker_id.is_some())
        {
            return Ok(());
        }
        // 选负载最低 worker 分配（记录 worker_id 到 region 映射）
        let worker_id = {
            let mut world = self.world.lock().expect("world lock");
            // worker 池未就绪（启动竞态：`Server::new` spawn worker 是异步的，
            // `WORKER_REGISTER` 尚未到达）时，等待 worker 注册而不是立即回退到
            // 直接 spawn——否则出生点 region 走兼容进程路径，卸载时又被 kill。
            let deadline = Instant::now() + Duration::from_secs(5);
            while world.workers.is_empty() && Instant::now() < deadline {
                drop(world);
                std::thread::sleep(Duration::from_millis(50));
                world = self.world.lock().expect("world lock");
            }
            let Some((worker_id, _)) = world.workers.assign_region() else {
                // 仍无可用 worker（worker 全部启动失败）：回退到直接 spawn region
                // 进程（兼容；正常环境 worker 池应在启动后数秒内注册完毕）
                debug!(id = %id.service_name(), "no worker available, direct spawn");
                let world_dir = world.world_dir.clone();
                world.regions.spawn(
                    &id,
                    &world_dir,
                    &self.config.shm_name,
                    self.config.log_level,
                )?;
                return Ok(());
            };
            worker_id
        };
        // 记录 region → worker 归属（region 条目先占位，注册后置 alive）
        self.world
            .lock()
            .expect("world lock")
            .regions
            .mark_assigned(&id, worker_id);
        let body = encode_worker_region(id.rx, id.rz);
        let worker_service = format!("icbm.worker.{worker_id}");
        if let Err(err) = self.ipc.send(&worker_service, kind::WORKER_ASSIGN, &body) {
            warn!(%err, worker_id, id = %id.service_name(), "worker assign failed");
        }
        Ok(())
    }

    /// 等待 region 完成注册（最多 10 秒）。
    fn wait_region_registered(&self, id: RegionId) -> Result<(), NetError> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let alive = self
                .world
                .lock()
                .expect("world lock")
                .regions
                .get(&id)
                .is_some_and(|entry| entry.alive);
            if alive {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(NetError::RegionTimeout)
    }

    /// 通知 region 玩家进入。
    pub fn notify_player_enter(&self, client: &Client, region: &RegionId) -> Result<(), NetError> {
        let position = *client.position.lock().expect("position");
        let body = icbm_region::protocol::encode_player_enter(
            &client.uuid.lock().expect("uuid"),
            [position.x, position.y, position.z],
            [position.yaw, position.pitch],
        );
        self.ipc
            .send(&region.service_name(), kind::PLAYER_ENTER, &body)
            .map_err(NetError::Ipc)
    }

    /// 通知 region 玩家离开。
    pub fn notify_player_leave(&self, client: &Client, region: &RegionId) -> Result<(), NetError> {
        let body = icbm_region::protocol::encode_player_leave(&client.uuid.lock().expect("uuid"));
        self.ipc
            .send(&region.service_name(), kind::PLAYER_LEAVE, &body)
            .map_err(NetError::Ipc)
    }

    /// 停止服务器（控制台 `stop`）。
    pub fn stop(&self) {
        self.inner.running.store(false, Ordering::Relaxed);
    }
}

impl std::ops::Deref for Server {
    type Target = ServerInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// 拉起嵌入式 `datarouter-switch`。
///
/// 新 datarouter 把 switch 二进制以 `include_bytes!` 嵌入本可执行文件；core 启动时
/// 直接 spawn（memfd 内存执行），不再需要操作者预先运行 switch。若同名 switch 已在
/// 运行（外部部署的旧工作流），则复用它并继续（不持有句柄）。
fn start_switch(
    config: &ServerConfig,
    auth_token: Option<String>,
) -> Result<Option<SwitchHandle>, ServerError> {
    let switch_config = SwitchConfig {
        shm_name: config.shm_name.clone(),
        mode: 0o600,
        ttl_ms: 5000,
        // core + 3×3 预生成（9 region）+ 扩容余量；datarouter 上限 64。
        max_ports: 32,
        // region 服务名动态（`icbm.region.<id>`），allowlist 无法穷举。
        allow_any: true,
        token: auth_token,
        reap_dead_processes: true,
        ..SwitchConfig::default()
    };
    match SwitchHandle::start(switch_config) {
        Ok(switch) => {
            info!("embedded datarouter-switch started on {}", config.shm_name);
            Ok(Some(switch))
        }
        Err(SwitchError::AlreadyRunning(name)) => {
            warn!(
                %name,
                "switch already running externally, reusing it"
            );
            Ok(None)
        }
        Err(SwitchError::StartTimedOut(_)) if shm_object_exists(&config.shm_name) => {
            // 同名共享内存对象已存在但探测失败：通常是旧版 datarouter 的外部
            // switch 残留（layout/通道大小不兼容），内嵌 switch 无法接管。
            Err(ServerError::Switch(SwitchError::InvalidConfig(format!(
                "shared memory {} is held by an incompatible switch; \
                 stop it and remove the object first: \
                 pkill -f datarouter-switch && rm -f /dev/shm/{}",
                config.shm_name,
                config.shm_name.trim_start_matches('/')
            ))))
        }
        Err(err) => Err(ServerError::Switch(err)),
    }
}

/// POSIX 共享内存对象是否已存在（Linux 下位于 /dev/shm）。
fn shm_object_exists(name: &str) -> bool {
    std::path::Path::new("/dev/shm")
        .join(name.trim_start_matches('/'))
        .exists()
}

/// 拉起 worker 进程池：`cpu_count - 2` 个常驻 worker（至少 1 个），
/// 平均绑定到除 core 自身之外的 CPU 核心（core 占用核心 0，worker i 绑核 i+1）。
///
/// worker 进程（`icbm --worker <id> --cpu <core>`）注册 `icbm.worker.<id>`，
/// 经 `WORKER_REGISTER` 由 core 记入 `WorkerRegistry`；region 由 `ensure_region`
/// 分配给负载最低的 worker（region 合并）。
fn spawn_worker_pool(config: &ServerConfig) {
    use crate::region_registry::level_filter_str;
    // core 自身占 1 核，其余给 worker；至少 1 个 worker（`ICBM_WORKERS` 可覆盖）
    let worker_count = crate::config::effective_worker_count(config);
    let cpu_count = std::thread::available_parallelism().map_or(1, usize::from);
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            warn!(%err, "cannot locate self executable, skipping worker pool");
            return;
        }
    };
    for id in 0..worker_count {
        let cpu = (id + 1) % cpu_count; // 核心 0 留给 core
        let mut command = std::process::Command::new(&exe);
        command
            .arg("--worker")
            .arg(id.to_string())
            .arg("--cpu")
            .arg(cpu.to_string())
            .arg("--world")
            .arg(&config.world_dir)
            .arg("--shm")
            .arg(&config.shm_name)
            .arg("--log-level")
            .arg(level_filter_str(config.log_level));
        match command.spawn() {
            Ok(child) => {
                debug!(id, pid = child.id(), cpu, "spawned worker process");
            }
            Err(err) => {
                warn!(%err, id, "failed to spawn worker");
            }
        }
    }
    info!(worker_count, cpu_count, "worker pool spawned");
}

/// datarouter receiver 线程：region 消息 + 生命周期维护。
fn spawn_receiver(server: &Server) {
    let server = server.clone();
    std::thread::spawn(move || {
        let mut last_icbm_push = Instant::now();
        let mut last_env_sync = Instant::now();
        while server.running.load(Ordering::Relaxed) {
            match server.ipc.recv(100) {
                Ok(Some(message)) => {
                    if message.kind == 0x07
                        && server
                            .chunk_diag
                            .fetch_add(1, Ordering::Relaxed)
                            .is_multiple_of(10)
                    {
                        // CHUNK_DATA 计数（诊断，每 10 个一条）
                        trace!(
                            count = server.chunk_diag.load(Ordering::Relaxed),
                            "core chunk data"
                        );
                    }
                    handle_core_message(&server, &message);
                }
                Ok(None) => {}
                Err(err) => {
                    warn!(%err, "ipc recv failed");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            maintain_regions(&server);
            // ICBM 快照约每秒推送（协议建议 refreshIntervalTicks=20）
            if last_icbm_push.elapsed() >= Duration::from_secs(1) {
                push_icbm_snapshots(&server);
                last_icbm_push = Instant::now();
            }
            // 全局时间/天气：每秒推进并广播给所有 region（core 管理，
            // region 不再各自推进，避免跨区域不一致）
            if last_env_sync.elapsed() >= Duration::from_secs(1) {
                sync_global_time_weather(&server);
                last_env_sync = Instant::now();
            }
        }
        info!("datarouter receiver stopped");
    });
}

/// 推进全局时间/天气并广播 `TIME_UPDATE`/`WEATHER_UPDATE` 给所有 region。
///
/// 对照 vanilla `MinecraftServer.tick`：主世界时间与天气由服务器统一推进，
/// region 只负责把收到的状态广播给各自玩家。
fn sync_global_time_weather(server: &Server) {
    let regions: Vec<icbm_region::RegionId> = {
        let world = server.world.lock().expect("world lock");
        world.regions.ids()
    };
    if regions.is_empty() {
        return;
    }
    let daylight = server
        .world
        .lock()
        .expect("world lock")
        .gamerules
        .get("doDaylightCycle")
        .is_none_or(|v| v == "true");
    let weather_cycle = server
        .world
        .lock()
        .expect("world lock")
        .gamerules
        .get("doWeatherCycle")
        .is_none_or(|v| v == "true");
    // 每秒推进 20 tick（与 region 20 TPS 一致）
    let mut time = server.world_time.lock().expect("world time");
    if daylight {
        *time = time.wrapping_add(20);
    }
    let time_value = *time;
    drop(time);
    let (raining, thundering, rain, thunder) = {
        let mut weather = server.weather.lock().expect("weather");
        for _ in 0..20 {
            let _ = weather.tick(weather_cycle);
        }
        (
            weather.is_raining(),
            weather.is_thundering(),
            weather.rain_gradient(),
            weather.thunder_gradient(),
        )
    };
    for region in regions {
        let _ = server.ipc.send(
            &region.service_name(),
            icbm_region::protocol::kind::TIME_UPDATE,
            &icbm_region::protocol::encode_time_update(time_value, false),
        );
        let _ = server.ipc.send(
            &region.service_name(),
            icbm_region::protocol::kind::WEATHER_UPDATE,
            &icbm_region::protocol::encode_weather_update(raining, thundering, rain, thunder),
        );
    }
}

/// 给已订阅且支持快照的玩家推送 ICBM 状态快照。
fn push_icbm_snapshots(server: &Server) {
    let players: Vec<Arc<Client>> = {
        let world = server.world.lock().expect("world lock");
        world.players.values().cloned().collect()
    };
    for client in players {
        if client.icbm_subscribed.load(Ordering::Relaxed)
            && client.icbm_snapshot_ok.load(Ordering::Relaxed)
        {
            crate::net::play::send_icbm_snapshot(server, &client);
        }
    }
}

/// region 心跳超时与沉睡卸载维护。
fn maintain_regions(server: &Server) {
    let now = Instant::now();
    let (timed_out, sleep_unloads) = {
        let world = server.world.lock().expect("world lock");
        (
            world
                .regions
                .timed_out(now, Duration::from_secs(server.config.region_timeout_secs)),
            world
                .regions
                .sleep_unloads(now, Duration::from_secs(server.config.sleep_unload_secs)),
        )
    };
    for id in timed_out {
        warn_timed_out(&id);
        let (pid, worker_id) = {
            let world = server.world.lock().expect("world lock");
            let entry = world.regions.get(&id);
            (entry.map(|e| e.pid), entry.and_then(|e| e.worker_id))
        };
        // 主动拆除失联实例：只删条目不停止旧实例会让它继续持有 datarouter
        // 注册（keepalive 使 TTL 永不回收），重分配时新线程 logon 报 BUSY
        // （"service is already registered"）→ 玩家注册超时被踢。
        stop_region_instance(server, id, pid, worker_id);
        server.world.lock().expect("world lock").regions.remove(&id);
    }
    for id in sleep_unloads {
        let (pid, worker_id) = {
            let world = server.world.lock().expect("world lock");
            let entry = world.regions.get(&id);
            (entry.map(|e| e.pid), entry.and_then(|e| e.worker_id))
        };
        let entity_base = server
            .world
            .lock()
            .expect("world lock")
            .regions
            .entity_base(&id);
        // 停止实例（worker 发 `WORKER_RELEASE`；direct spawn 发 UNLOAD + 收割/杀）
        stop_region_instance(server, id, pid, worker_id);
        server.world.lock().expect("world lock").regions.remove(&id);
        // 通知在线玩家移除该区域的实体（客户端不感知 region 卸载，需显式 REMOVE_ENTITIES）
        if let Some(base) = entity_base {
            remove_region_entities(server, base);
        }
        info!(id = %id.service_name(), worker_id, "region unloaded after sleep");
    }
}

/// 停止一个 region 实例（心跳超时/沉睡卸载共用）。
///
/// - worker 宿主：发 `WORKER_RELEASE`（worker 内停止 region 线程并 flush，进程
///   存活；线程 Drop 时 datarouter 注销，重分配不再撞 BUSY）。
/// - 直接 spawn（无 worker 的兼容路径）：发 `REGION_UNLOAD` 等退出，先收割
///   zombie（未收割子进程的 `kill(pid,0)` 恒真），仍存活才 SIGTERM。
fn stop_region_instance(server: &Server, id: RegionId, pid: Option<u32>, worker_id: Option<u32>) {
    if let Some(worker_id) = worker_id {
        let worker_service = format!("icbm.worker.{worker_id}");
        let body = encode_worker_region(id.rx, id.rz);
        let _ = server
            .ipc
            .send(&worker_service, kind::WORKER_RELEASE, &body);
        // 释放 worker 负载计数
        server
            .world
            .lock()
            .expect("world lock")
            .workers
            .release_region(worker_id);
    } else if let Some(pid) = pid {
        // 兼容回退：直接 spawn 的 region（无 worker），发 UNLOAD + 等退出
        let _ = server.ipc.send(
            &id.service_name(),
            kind::REGION_UNLOAD,
            &encode_unload(DIM_OVERWORLD, id.rx, id.rz),
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && process_alive(pid) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if process_alive(pid) {
            warn!(id = %id.service_name(), pid, "region did not exit after unload, killing");
            let _ = kill_process(pid);
        }
    }
}

/// 向所有在线玩家发送 `REMOVE_ENTITIES`，清除一个 region 的实体（卸载信号）。
fn remove_region_entities(server: &Server, entity_base: i32) {
    let ids: Vec<i32> =
        (entity_base..entity_base + crate::region_registry::ENTITY_ID_STRIDE).collect();
    let players: Vec<Arc<crate::client::Client>> = {
        let world = server.world.lock().expect("world lock");
        world.players.values().cloned().collect()
    };
    for client in players {
        let remove = CRemoveEntities {
            entity_ids: ids.clone(),
        };
        let _ = client.send_encoded(CRemoveEntities::packet_id(), |w| remove.encode(w));
    }
}

/// 处理一条来自 region 的消息。
#[allow(clippy::too_many_lines)] // IPC 消息分发 match 逐条对应协议
fn handle_core_message(server: &Server, message: &icbm_region::protocol::IpcMessage) {
    match message.kind {
        kind::WORKER_REGISTER => {
            let Ok((pid, cpu)) = decode_worker_register(&message.body) else {
                warn!("malformed worker register");
                return;
            };
            // worker 服务名 `icbm.worker.<id>`
            let Some(id) = parse_worker_service_id(&message.source) else {
                warn!(source = %message.source, "worker register from unknown source");
                return;
            };
            let first =
                server
                    .world
                    .lock()
                    .expect("world lock")
                    .workers
                    .register(id, pid, u32::from(cpu));
            debug!(id, pid, cpu, first, "worker registered");
        }
        kind::REGION_REGISTER => {
            let Ok((dim, rx, rz, pid)) = decode_register(&message.body) else {
                warn!("malformed register message");
                return;
            };
            let id = RegionId { rx, rz };
            let known = server
                .world
                .lock()
                .expect("world lock")
                .regions
                .contains(&id);
            if known {
                server
                    .world
                    .lock()
                    .expect("world lock")
                    .regions
                    .mark_registered(&id);
                let entity_base = server
                    .world
                    .lock()
                    .expect("world lock")
                    .regions
                    .entity_base(&id)
                    .unwrap_or(0);
                let _ = server.ipc.send(
                    &message.source,
                    kind::REGISTER_ACK,
                    &encode_register_ack(dim, rx, rz, true, entity_base),
                );
                debug!(id = %id.service_name(), pid, entity_base, "region registered");
            } else {
                let _ = server.ipc.send(
                    &message.source,
                    kind::REGISTER_ACK,
                    &encode_register_ack(dim, rx, rz, false, 0),
                );
                warn!(id = %id.service_name(), "rejected unknown region");
            }
        }
        kind::REGION_HEARTBEAT => {
            let Ok((tps, sleeping, loaded, tick)) = decode_heartbeat(&message.body) else {
                return;
            };
            let Some(id) = parse_region_service_name(&message.source) else {
                return;
            };
            server
                .world
                .lock()
                .expect("world lock")
                .regions
                .update_heartbeat(
                    &id,
                    tps,
                    sleeping,
                    loaded,
                    u32::try_from(tick).unwrap_or(u32::MAX),
                );
        }
        kind::CHUNK_DATA => forward_chunk_data(server, &message.body),
        kind::ENTITY_DATA => forward_entity_data(server, &message.body),
        kind::BLOCK_QUERY_RESP => {
            if let Ok((_dim, x, y, z, state)) = decode_block_query_resp(&message.body) {
                // 找到查询该位置的玩家（简化：仅限最近查询传送门的玩家——
                // 通过遍历玩家位置匹配）
                let players: Vec<Arc<crate::client::Client>> = {
                    let world = server.world.lock().expect("world lock");
                    world.players.values().cloned().collect()
                };
                for player in players {
                    let pos = player.position.lock().expect("position");
                    let px = pos.x.floor() as i32;
                    let py = pos.y.floor() as i32;
                    let pz = pos.z.floor() as i32;
                    let matches = (px == x && pz == z) && (py == y || py - 1 == y);
                    drop(pos);
                    if !matches {
                        continue;
                    }
                    // 屏障检测优先：路由到 pending_barrier_state；其次传送门检测
                    if player
                        .barrier_query_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        player
                            .pending_barrier_state
                            .store(state, std::sync::atomic::Ordering::Relaxed);
                    } else if player
                        .portal_query_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        player
                            .pending_portal_block
                            .store(state, std::sync::atomic::Ordering::Relaxed);
                    }
                    break;
                }
            }
        }
        kind::REGION_STATS_RESP => {
            // Phase 1：心跳已含统计；统计响应暂不消费
        }
        kind::BLOCK_NBT_RESP => {
            if let Ok((_dim, x, y, z, nbt)) = decode_block_nbt_resp(&message.body) {
                // 路由到挂起的 /data 请求玩家（receiver 线程）
                let players: Vec<Arc<crate::client::Client>> = {
                    let world = server.world.lock().expect("world lock");
                    world.players.values().cloned().collect()
                };
                for player in players {
                    if player
                        .data_request_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        *player.pending_block_nbt.lock().expect("block nbt") = nbt;
                        let _ = (x, y, z);
                        break;
                    }
                }
            }
        }
        kind::COMMAND_EXECUTE => {
            if let Ok(command) = decode_command_execute(&message.body) {
                // 命令方块触发：以控制台身份执行（反馈仅记录日志）
                debug!(source = %message.source, %command, "command block executed via core");
                server.handle_command(&format!("/{command}"));
            }
        }
        kind::BLOCK_RAYCAST_RESP => {
            if let Ok((hit, x, y, z)) = decode_block_raycast_resp(&message.body) {
                // 路由到挂起末影珍珠检测的玩家（receiver 线程）
                let players: Vec<Arc<crate::client::Client>> = {
                    let world = server.world.lock().expect("world lock");
                    world.players.values().cloned().collect()
                };
                for player in players {
                    if player
                        .raycast_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        *player.pending_raycast.lock().expect("raycast") = Some((hit, x, y, z));
                        break;
                    }
                }
            }
        }
        kind::SURFACE_RESP => {
            if let Ok(y) = decode_surface_resp(&message.body) {
                let players: Vec<Arc<crate::client::Client>> = {
                    let world = server.world.lock().expect("world lock");
                    world.players.values().cloned().collect()
                };
                for player in players {
                    if player
                        .surface_query_pending
                        .swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        *player.pending_surface.lock().expect("surface") = Some(y);
                        break;
                    }
                }
            }
        }
        other => {
            warn!(kind = other, source = %message.source, "unhandled core message");
        }
    }
}

/// 转发区块数据给等待中的玩家（含批次包装）。
fn forward_chunk_data(server: &Server, body: &[u8]) {
    let Ok((_dim, cx, cz, data)) = decode_chunk_data(body) else {
        return;
    };
    // 取等待者
    let player = {
        let world = server.world.lock().expect("world lock");
        world.chunk_pending.as_ref().map(|pending| pending.player)
    };
    let Some(player) = player else { return };
    let client = {
        let world = server.world.lock().expect("world lock");
        world.players.get(&player).cloned()
    };
    let Some(client) = client else {
        server.world.lock().expect("world lock").chunk_pending = None;
        return;
    };
    forward_chunk_to_pending(server, &client, cx, cz, data);
}

/// 转发单个区块载荷给等待中的玩家（批次开始 → 区块 → 批次完成）。
///
/// 供 region 返回的 `CHUNK_DATA`（receiver 线程）与 core 直读已卸载 region
/// `.mca` 的区块（请求线程）共用：pending 计数、请求区域过滤与批次包装统一
/// 在这里完成。
pub(crate) fn forward_chunk_to_pending(
    server: &Server,
    client: &crate::client::Client,
    cx: i32,
    cz: i32,
    data: &[u8],
) {
    // 预生成/预加载区块不在当前请求区域内：不转发、不计数（丢弃）
    {
        let world = server.world.lock().expect("world lock");
        if let Some(pending) = world.chunk_pending.as_ref()
            && pending.player == *client.uuid.lock().expect("uuid")
            && !pending.contains(cx, cz)
        {
            return;
        }
    }

    // 更新 pending 状态（短锁）
    let (needs_start, finish, total) = {
        let mut world = server.world.lock().expect("world lock");
        let Some(pending) = world.chunk_pending.as_mut() else {
            return;
        };
        let needs_start = !pending.in_batch;
        pending.in_batch = true;
        pending.received += 1;
        let finish = pending.received >= pending.total;
        (needs_start, finish, pending.total)
    };

    // 发送（顺序：批次开始 → 区块 → 批次完成）
    let start_ok = !needs_start
        || client
            .send_encoded(CChunkBatchStart::packet_id(), |w| {
                CChunkBatchStart.encode(w);
            })
            .is_ok();
    let send_ok = start_ok
        && client
            .send_packet(play_ids::CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT, data)
            .is_ok();

    if !send_ok {
        warn!(
            "chunk forwarding failed: start_ok={start_ok} data_len={} needs_start={needs_start}",
            data.len()
        );
        server.world.lock().expect("world lock").chunk_pending = None;
        return;
    }
    if finish {
        let batch_size = i32::try_from(total).unwrap_or(i32::MAX);
        let _ = client.send_encoded(CChunkBatchFinished::packet_id(), |w| {
            CChunkBatchFinished { batch_size }.encode(w);
        });
        server.world.lock().expect("world lock").chunk_pending = None;
    }
}

/// 转发 region 的实体包给位于该区域的玩家（`ENTITY_DATA`）。
fn forward_entity_data(server: &Server, body: &[u8]) {
    let Ok((_dim, rx, rz, packet_id, payload)) = decode_entity_data(body) else {
        return;
    };
    let players: Vec<Arc<crate::client::Client>> = {
        let world = server.world.lock().expect("world lock");
        world
            .player_regions
            .iter()
            .filter(|(_, region)| region.rx == rx && region.rz == rz)
            .filter_map(|(uuid, _)| world.players.get(uuid).cloned())
            .collect()
    };
    for client in players {
        let _ = client.send_packet(packet_id, payload);
    }
}

/// 从服务名解析 region ID（`icbm.region.o_<rx>_<rz>`）。
fn parse_region_service_name(name: &str) -> Option<RegionId> {
    let rest = name.strip_prefix("icbm.region.o_")?;
    let (rx, rz) = rest.split_once('_')?;
    Some(RegionId {
        rx: rx.parse().ok()?,
        rz: rz.parse().ok()?,
    })
}

/// 从服务名解析 worker ID（`icbm.worker.<id>`）。
fn parse_worker_service_id(name: &str) -> Option<u32> {
    name.strip_prefix("icbm.worker.")?.parse().ok()
}

/// 终止 region 子进程（直接发 SIGTERM 信号，不依赖外部 `kill` 命令——
/// 保持 release 单二进制自包含，可在最小 Linux 环境运行）。
///
/// `pid == 0` 表示 worker 占位条目（未记真实进程），发送 SIGTERM 到进程组
/// 会误伤整组进程——防御性跳过。
fn kill_process(pid: u32) -> std::io::Result<()> {
    if pid == 0 {
        return Ok(());
    }
    // SAFETY: 仅向已知子进程 PID 发送 SIGTERM；pid 合法则必有返回。
    let result = unsafe { libc::kill(i32::try_from(pid).unwrap_or(-1), libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 收割已退出但未被 `waitpid` 的子进程（zombie）。
///
/// Rust `Command::spawn` 丢弃 `Child` 后不自动收割，退出进程留 zombie；
/// 此时 `kill(pid, 0)` 仍返回成功（进程存在），导致 compat 卸载路径恒判
/// "未退出"。用 `waitpid(WNOHANG)` 收割后再判断。
fn reap_zombie(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: WNOHANG 不阻塞；pid 为已知子进程，回收其退出状态。
    let mut status: libc::c_int = 0;
    unsafe {
        libc::waitpid(
            i32::try_from(pid).unwrap_or(-1),
            &raw mut status,
            libc::WNOHANG,
        );
    }
}

/// 进程是否仍存活（`kill(pid, 0)`；权限错误视为存活）。
///
/// 先收割 zombie：`kill(pid,0)` 对僵尸返回成功，会误判已退出进程为存活。
fn process_alive(pid: u32) -> bool {
    reap_zombie(pid);
    if pid == 0 {
        return false;
    }
    // SAFETY: 仅查询信号 0（不发送）；pid 为已知子进程。
    let result = unsafe { libc::kill(i32::try_from(pid).unwrap_or(-1), 0) };
    result == 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
}

#[cfg(test)]
mod tests {
    use super::parse_region_service_name;
    use icbm_region::RegionId;

    #[test]
    fn service_name_parsing() {
        assert_eq!(
            parse_region_service_name("icbm.region.o_0_0"),
            Some(RegionId { rx: 0, rz: 0 })
        );
        assert_eq!(
            parse_region_service_name("icbm.region.o_-3_5"),
            Some(RegionId { rx: -3, rz: 5 })
        );
        assert_eq!(parse_region_service_name("icbm.core"), None);
        assert_eq!(parse_region_service_name("icbm.region.n_0_0"), None);
    }

    #[test]
    fn zombie_process_is_reaped_and_detected_dead() {
        use std::process::{Command, Stdio};
        // 子进程立即退出，句柄丢弃（不 wait）→ 变 zombie；`process_alive`
        // 应先 `waitpid(WNOHANG)` 收割再判定死亡，避免 compat 卸载路径恒判
        // "未退出" 并误发 SIGTERM（pid 复用风险）。
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        drop(child);
        // 等子进程退出成 zombie
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !super::process_alive(pid),
            "zombie child should be reaped and reported dead"
        );
        // pid=0 防御：不指向任何进程
        assert!(!super::process_alive(0));
        assert!(super::kill_process(0).is_ok());
    }
}
