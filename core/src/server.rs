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
    DIM_OVERWORLD, Ipc, IpcError, decode_chunk_data, decode_entity_data, decode_heartbeat,
    decode_register, encode_register_ack, encode_unload, kind,
};
use tracing::{info, trace, warn};

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
    /// 收到的 `CHUNK_DATA` 计数（诊断）。
    pub chunk_diag: std::sync::atomic::AtomicU64,
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
        let world = Mutex::new(WorldState::new(config.world_dir.clone()));
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
                key_store: crate::net::crypto::KeyStore::new()
                    .map_err(ServerError::Crypto)?,
                chunk_diag: std::sync::atomic::AtomicU64::new(0),
            }),
        })
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
                Err(err) => {
                    warn!(%err, "accept failed");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }

        // 停止：断开玩家、退出 region、等待连接线程
        let mut world = self.world.lock().expect("world lock");
        for client in world.players.values() {
            client.alive.store(false, Ordering::Relaxed);
            client.kick("服务器关闭");
        }
        world.players.clear();
        // 连接线程在 read timeout（100ms）内观察到 running=false 后退出
        for handle in handles {
            handle.join().ok();
        }
        Ok(())
    }

    /// 确保 region 已拉起并注册（无则等待最多 10 秒）。
    pub fn ensure_region(&self, id: RegionId) -> Result<(), NetError> {
        let world_dir = self.world.lock().expect("world lock").world_dir.clone();
        let spawn_result = {
            let mut world = self.world.lock().expect("world lock");
            world.regions.spawn(
                &id,
                &world_dir,
                &self.config.shm_name,
                self.config.log_level,
            )
        };
        spawn_result.map_err(NetError::Io)?;
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

/// datarouter receiver 线程：region 消息 + 生命周期维护。
fn spawn_receiver(server: &Server) {
    let server = server.clone();
    std::thread::spawn(move || {
        let mut last_icbm_push = Instant::now();
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
        }
        info!("datarouter receiver stopped");
    });
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
        let mut world = server.world.lock().expect("world lock");
        world.regions.remove(&id);
    }
    for id in sleep_unloads {
        let pid = server
            .world
            .lock()
            .expect("world lock")
            .regions
            .get(&id)
            .map(|entry| entry.pid);
        let entity_base = server
            .world
            .lock()
            .expect("world lock")
            .regions
            .entity_base(&id);
        let _ = server.ipc.send(
            &id.service_name(),
            kind::REGION_UNLOAD,
            &encode_unload(DIM_OVERWORLD, id.rx, id.rz),
        );
        if let Some(pid) = pid {
            let _ = kill_process(pid);
        }
        server.world.lock().expect("world lock").regions.remove(&id);
        // 通知在线玩家移除该区域的实体（客户端不感知 region 卸载，需显式 REMOVE_ENTITIES）
        if let Some(base) = entity_base {
            remove_region_entities(server, base);
        }
        info!(id = %id.service_name(), "region unloaded after sleep");
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
fn handle_core_message(server: &Server, message: &icbm_region::protocol::IpcMessage) {
    match message.kind {
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
                info!(id = %id.service_name(), pid, entity_base, "region registered");
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
        kind::REGION_STATS_RESP => {
            // Phase 1：心跳已含统计；统计响应暂不消费
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
    // 预生成/预加载区块不在当前请求区域内：不转发、不计数（丢弃）
    {
        let world = server.world.lock().expect("world lock");
        if let Some(pending) = world.chunk_pending.as_ref()
            && pending.player == player
            && !pending.contains(cx, cz)
        {
            return;
        }
    }
    let client = {
        let world = server.world.lock().expect("world lock");
        world.players.get(&player).cloned()
    };
    let Some(client) = client else {
        server.world.lock().expect("world lock").chunk_pending = None;
        return;
    };

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

/// 终止 region 子进程（直接发 SIGTERM 信号，不依赖外部 `kill` 命令——
/// 保持 release 单二进制自包含，可在最小 Linux 环境运行）。
fn kill_process(pid: u32) -> std::io::Result<()> {
    // SAFETY: 仅向已知子进程 PID 发送 SIGTERM；pid 合法则必有返回。
    let result = unsafe { libc::kill(i32::try_from(pid).unwrap_or(-1), libc::SIGTERM) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
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
}
