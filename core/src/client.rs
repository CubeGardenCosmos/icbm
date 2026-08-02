//! 客户端连接：状态机（握手/状态/登录/配置/Play）与包处理。
//!
//! 每连接一个线程；`TcpStream` 读在连接线程独占，写经 `Mutex` 供
//! datarouter receiver 线程转发区块。

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use icbm_gameplay_minecraft::commands::GameMode;
use icbm_mc_767::ConnectionState;
use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::packets::decode_frame;
use icbm_mc_767::packets::handshake::SIntent;
use icbm_mc_767::packets::login::{
    CLoginDisconnect, CLoginSuccess, CSetCompression, SLoginAcknowledged, SLoginStart,
};
use icbm_mc_767::packets::play::clientbound::{
    CGameEvent, CPlayerAbilities, CPlayerInfoUpdate, CPlayerPosition, CSetChunkCacheCenter,
    CSetChunkCacheRadius, CSetDefaultSpawnPosition, PlayerInfoEntry,
};
use icbm_mc_767::packets::play_ids;
use icbm_mc_767::packets::status::{CPongResponse, CStatusResponse, SPingRequest, SStatusRequest};
use icbm_mc_767::packets::status_ids;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::net::play::handle_play_packet;
use crate::net::status::status_response;
use crate::server::Server;

/// 网络错误。
#[derive(Debug)]
pub enum NetError {
    /// IO 错误。
    Io(std::io::Error),
    /// 协议帧错误。
    Frame(icbm_mc_767::packets::FrameError),
    /// 编解码错误。
    Codec(icbm_mc_767::codec::CodecError),
    /// 协议版本不支持。
    BadProtocol(i32),
    /// 非法的包（状态/ID）。
    BadPacket(&'static str),
    /// region 拉起或注册超时。
    RegionTimeout,
    /// datarouter IPC 错误。
    Ipc(icbm_region::protocol::IpcError),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io: {err}"),
            Self::Frame(err) => write!(f, "frame: {err}"),
            Self::Codec(err) => write!(f, "codec: {err}"),
            Self::BadProtocol(version) => write!(f, "unsupported protocol version {version}"),
            Self::BadPacket(msg) => write!(f, "bad packet: {msg}"),
            Self::RegionTimeout => f.write_str("region spawn or registration timeout"),
            Self::Ipc(err) => write!(f, "ipc: {err}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<icbm_mc_767::packets::FrameError> for NetError {
    fn from(value: icbm_mc_767::packets::FrameError) -> Self {
        Self::Frame(value)
    }
}

impl From<icbm_mc_767::codec::CodecError> for NetError {
    fn from(value: icbm_mc_767::codec::CodecError) -> Self {
        Self::Codec(value)
    }
}

impl From<icbm_region::protocol::IpcError> for NetError {
    fn from(value: icbm_region::protocol::IpcError) -> Self {
        Self::Ipc(value)
    }
}

/// 玩家位置。
#[derive(Debug, Clone, Copy)]
pub struct Position {
    /// X。
    pub x: f64,
    /// Y。
    pub y: f64,
    /// Z。
    pub z: f64,
    /// 偏航角。
    pub yaw: f32,
    /// 俯仰角。
    pub pitch: f32,
}

impl Position {
    /// 玩家所在区块 X。
    #[must_use]
    pub const fn chunk_x(&self) -> i32 {
        self.x.floor() as i32 >> 4
    }

    /// 玩家所在区块 Z。
    #[must_use]
    pub const fn chunk_z(&self) -> i32 {
        self.z.floor() as i32 >> 4
    }
}

/// 客户端（共享引用，receiver 线程可转发区块）。
pub struct Client {
    /// 玩家 UUID（登录前为 nil）。
    pub uuid: Mutex<Uuid>,
    /// 玩家名（登录前为空）。
    pub name: Mutex<String>,
    /// 写流（发送锁）。
    write: Mutex<TcpStream>,
    /// 压缩阈值（`None` = 未启用）。
    compression: Mutex<Option<i32>>,
    /// 连接状态。
    pub state: AtomicU8,
    /// 游戏模式。
    pub gamemode: Mutex<GameMode>,
    /// 位置。
    pub position: Mutex<Position>,
    /// 可视距离。
    pub view_distance: AtomicI32,
    /// 是否存活（连接可继续）。
    pub alive: AtomicBool,
    /// 待确认的 keep alive。
    keep_alive: Mutex<Option<(i64, Instant)>>,
    /// 传送 ID 计数。
    teleport_id: AtomicI32,
    /// 延迟（毫秒）。
    pub latency: AtomicI32,
}

const STATE_HANDSHAKE: u8 = 0;
const STATE_STATUS: u8 = 1;
const STATE_LOGIN: u8 = 2;
const STATE_CONFIG: u8 = 3;
const STATE_PLAY: u8 = 4;

impl Client {
    /// 新建客户端。
    #[must_use]
    pub fn new(uuid: Uuid, name: String, write: TcpStream) -> Arc<Self> {
        Arc::new(Self {
            uuid: Mutex::new(uuid),
            name: Mutex::new(name),
            write: Mutex::new(write),
            compression: Mutex::new(None),
            state: AtomicU8::new(STATE_HANDSHAKE),
            gamemode: Mutex::new(GameMode::Survival),
            position: Mutex::new(Position {
                x: 0.0,
                y: 4.0,
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
            }),
            view_distance: AtomicI32::new(8),
            alive: AtomicBool::new(true),
            keep_alive: Mutex::new(None),
            teleport_id: AtomicI32::new(0),
            latency: AtomicI32::new(0),
        })
    }

    /// 连接状态。
    #[must_use]
    pub fn connection_state(&self) -> ConnectionState {
        match self.state.load(Ordering::Relaxed) {
            STATE_STATUS => ConnectionState::Status,
            STATE_LOGIN => ConnectionState::Login,
            STATE_CONFIG => ConnectionState::Config,
            STATE_PLAY => ConnectionState::Play,
            _ => ConnectionState::Handshake,
        }
    }

    fn set_compression(&self, threshold: i32) {
        *self.compression.lock().expect("compression lock") = Some(threshold);
    }

    #[must_use]
    fn compression_threshold(&self) -> Option<i32> {
        *self.compression.lock().expect("compression lock")
    }

    /// 发送一个完整包（自动帧编码与压缩）。
    pub fn send_packet(&self, packet_id: i32, payload: &[u8]) -> Result<(), NetError> {
        let frame =
            icbm_mc_767::packets::encode_frame(packet_id, payload, self.compression_threshold())?;
        let mut write = self.write.lock().expect("write lock");
        write.write_all(&frame)?;
        Ok(())
    }

    /// 发送仅由编码函数定义的包。
    pub fn send_encoded(
        &self,
        packet_id: i32,
        encode: impl FnOnce(&mut Writer),
    ) -> Result<(), NetError> {
        let mut writer = Writer::new();
        encode(&mut writer);
        self.send_packet(packet_id, &writer.into_bytes())
    }

    /// 断开连接（发送原因并关闭）。
    ///
    /// 原因按官方 1.20.5+ 格式编码：组件网络序列化为 NBT `{text: ...}`
    /// （客户端 `FriendlyByteBuf.readNbt` 解析），非裸 JSON 字符串。
    pub fn kick(&self, reason: &str) {
        // 各阶段 disconnect 包 ID 不同（Mojang：login=0x00/config=0x02/play=0x1D）
        let id = match self.connection_state() {
            ConnectionState::Login => icbm_mc_767::packets::login_ids::CLIENTBOUND_DISCONNECT,
            ConnectionState::Config => icbm_mc_767::packets::config_ids::CLIENTBOUND_DISCONNECT,
            _ => icbm_mc_767::packets::play_ids::CLIENTBOUND_DISCONNECT,
        };
        let _ = self.send_encoded(id, |w| w.write_text_component(reason));
        self.alive.store(false, Ordering::Relaxed);
    }

    /// 发送系统聊天。
    pub fn send_system_chat(&self, message: &str, overlay: bool) -> Result<(), NetError> {
        let json = serde_json::json!({ "text": message }).to_string();
        self.send_encoded(play_ids::CLIENTBOUND_SYSTEM_CHAT, |w| {
            w.write_string(&json);
            w.write_bool(overlay);
        })
    }

    /// 记录待确认 keep alive。
    fn set_keep_alive(&self, id: i64) {
        *self.keep_alive.lock().expect("keep alive lock") = Some((id, Instant::now()));
    }

    /// 待确认 keep alive 是否超时。
    fn keep_alive_timed_out(&self, timeout: Duration) -> bool {
        self.keep_alive
            .lock()
            .expect("keep alive lock")
            .is_some_and(|(_, sent)| sent.elapsed() > timeout)
    }
}

/// 处理单个连接（阻塞直到断开）。
pub fn handle_connection(server: &Server, mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    let _ = stream.set_nodelay(true);

    let mut buffer: Vec<u8> = Vec::new();
    let mut last_keep_alive = Instant::now();
    let mut keep_alive_id: i64 = 0;
    let mut client: Option<Arc<Client>> = None;

    loop {
        // 读一块
        let mut chunk = [0u8; 8192];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }

        // 解帧并处理
        if let Some(client_ref) = &client {
            loop {
                let threshold = client_ref.compression_threshold();
                match decode_frame(&buffer, threshold) {
                    Ok(Some((id, payload, consumed))) => {
                        buffer.drain(..consumed);
                        if let Err(err) = handle_serverbound(server, client_ref, id, &payload) {
                            debug!(%err, "handling packet failed");
                            client_ref.kick("数据包处理失败");
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        warn!(%err, "frame decode error");
                        client_ref.kick("协议帧错误");
                        break;
                    }
                }
                if !client_ref.alive.load(Ordering::Relaxed) {
                    break;
                }
            }
        }

        let Some(client_ref) = &client else {
            // 尚未建立 Client（握手前）：首包必须是握手（无压缩）
            if buffer.is_empty() {
                if !server.running.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
            let Ok(Some((0x00, payload, consumed))) = decode_frame(&buffer, None) else {
                break;
            };
            buffer.drain(..consumed);
            match SIntent::decode(&mut Reader::new(&payload)) {
                Ok(intent) => match init_connection(server, &stream, &intent) {
                    Ok(Some(created)) => client = Some(created),
                    Ok(None) => break,
                    Err(err) => {
                        debug!(%err, "connection init failed");
                        break;
                    }
                },
                Err(_) => break,
            }
            continue;
        };

        // keep alive（Play 阶段）
        if client_ref.connection_state() == ConnectionState::Play
            && last_keep_alive.elapsed() >= Duration::from_secs(5)
        {
            keep_alive_id += 1;
            let id = keep_alive_id;
            let _ = client_ref.send_encoded(play_ids::CLIENTBOUND_KEEP_ALIVE, |w| {
                w.write_i64_be(id);
            });
            client_ref.set_keep_alive(id);
            last_keep_alive = Instant::now();
        }
        if client_ref.keep_alive_timed_out(Duration::from_secs(30)) {
            let name = client_ref.name.lock().expect("name").clone();
            info!(name = %name, "keep alive timeout, kicking");
            client_ref.kick("心跳超时");
            break;
        }
        if !server.running.load(Ordering::Relaxed) {
            break;
        }
    }

    if let Some(client_ref) = client {
        on_disconnect(server, &client_ref);
    }
}

/// 握手：协议校验与初始 Client 建立。
fn init_connection(
    server: &Server,
    stream: &TcpStream,
    intent: &SIntent,
) -> Result<Option<Arc<Client>>, NetError> {
    if intent.protocol_version != icbm_mc_767::protocol::PROTOCOL_VERSION {
        return Err(NetError::BadProtocol(intent.protocol_version));
    }
    match intent.next_state {
        1 => {
            // 状态阶段
            let client = Client::new(Uuid::nil(), String::new(), stream.try_clone()?);
            client.state.store(STATE_STATUS, Ordering::Relaxed);
            let _ = server;
            Ok(Some(client))
        }
        2 => {
            // 登录阶段
            let client = Client::new(Uuid::nil(), String::new(), stream.try_clone()?);
            client.state.store(STATE_LOGIN, Ordering::Relaxed);
            let _ = server;
            Ok(Some(client))
        }
        _ => Err(NetError::BadPacket("unknown next state")),
    }
}

/// 服务端包分发。
fn handle_serverbound(
    server: &Server,
    client: &Arc<Client>,
    id: i32,
    payload: &[u8],
) -> Result<(), NetError> {
    let mut reader = Reader::new(payload);
    match client.connection_state() {
        ConnectionState::Status => match id {
            status_ids::SERVERBOUND_REQUEST => {
                let _ = SStatusRequest::decode(&mut reader)?;
                handle_status_request(server, client)?;
            }
            status_ids::SERVERBOUND_PING => {
                let ping = SPingRequest::decode(&mut reader)?;
                client.send_encoded(CPongResponse::packet_id(), |w| {
                    CPongResponse {
                        payload: ping.payload,
                    }
                    .encode(w);
                })?;
                client.alive.store(false, Ordering::Relaxed); // 响应后关闭
            }
            _ => return Err(NetError::BadPacket("unexpected status packet")),
        },
        ConnectionState::Login => match id {
            icbm_mc_767::packets::login_ids::SERVERBOUND_START => {
                let start = SLoginStart::decode(&mut reader)?;
                handle_login_start(server, client, start)?;
            }
            icbm_mc_767::packets::login_ids::SERVERBOUND_ACKNOWLEDGED => {
                let _ = SLoginAcknowledged::decode(&mut reader)?;
                client.state.store(STATE_CONFIG, Ordering::Relaxed);
                let name = client.name.lock().expect("name").clone();
                debug!(name = %name, "login acknowledged, entering config");
                send_config_intro(server, client)?;
            }
            _ => return Err(NetError::BadPacket("unexpected login packet")),
        },
        ConnectionState::Config => match id {
            icbm_mc_767::packets::config_ids::SERVERBOUND_CLIENT_INFORMATION => {
                let info = icbm_mc_767::packets::config::SClientInformation::decode(&mut reader)?;
                client
                    .view_distance
                    .store(i32::from(info.view_distance), Ordering::Relaxed);
            }
            icbm_mc_767::packets::config_ids::SERVERBOUND_KNOWN_PACKS => {
                let _ = icbm_mc_767::packets::config::SKnownPacks::decode(&mut reader)?;
                // 客户端回显已知资源包后，服务器下发注册表/标签并完成配置。
                send_config_data(client)?;
            }
            icbm_mc_767::packets::config_ids::SERVERBOUND_FINISH_CONFIGURATION => {
                let _ = icbm_mc_767::packets::config::SFinishConfig::decode(&mut reader)?;
                // 官方：服务器已通过 ReadyS2CPacket 发送完成配置（transitionsNetworkState），
                // 客户端回 ReadyC2SPacket 后双方进入 Play；此处不再回包。
                client.state.store(STATE_PLAY, Ordering::Relaxed);
                let name = client.name.lock().expect("name").clone();
                info!(name = %name, "entering play");
                enter_play(server, client)?;
            }
            icbm_mc_767::packets::config_ids::SERVERBOUND_KEEP_ALIVE => {
                let _ = icbm_mc_767::packets::config::SKeepAlive::decode(&mut reader)?;
            }
            icbm_mc_767::packets::config_ids::SERVERBOUND_PONG => {
                let _ = icbm_mc_767::packets::config::SPong::decode(&mut reader)?;
            }
            icbm_mc_767::packets::config_ids::SERVERBOUND_CUSTOM_PAYLOAD => {
                let _ = icbm_mc_767::packets::config::SPluginMessage::decode(&mut reader)?;
            }
            _ => return Err(NetError::BadPacket("unexpected config packet")),
        },
        ConnectionState::Play => handle_play_packet(server, client, id, &mut reader)?,
        ConnectionState::Handshake => return Err(NetError::BadPacket("handshake already done")),
    }
    Ok(())
}

/// 状态响应（MOTD + favicon）。
fn handle_status_request(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    let online = server.world.lock().expect("world lock").players.len() as i32;
    let response = status_response(&server.config.motd, server.config.max_players, online, &[]);
    client.send_encoded(CStatusResponse::packet_id(), |w| {
        CStatusResponse { response }.encode(w);
    })?;
    Ok(())
}

/// 登录开始：离线 UUID → 压缩 → 登录成功。
fn handle_login_start(
    server: &Server,
    client: &Arc<Client>,
    start: SLoginStart,
) -> Result<(), NetError> {
    let name = start.name;
    if name.is_empty() || name.len() > 16 {
        client.send_encoded(CLoginDisconnect::packet_id(), |w| {
            CLoginDisconnect {
                reason: "玩家名长度非法".to_owned(),
            }
            .encode(w);
        })?;
        client.alive.store(false, Ordering::Relaxed);
        return Ok(());
    }
    let uuid = offline_uuid(&name);
    *client.uuid.lock().expect("uuid") = uuid;
    *client.name.lock().expect("name") = name;

    // 压缩
    let threshold = server.config.compression_threshold;
    if threshold > 0 {
        client.send_encoded(CSetCompression::packet_id(), |w| {
            CSetCompression { threshold }.encode(w);
        })?;
        client.set_compression(threshold);
    }
    // 登录成功
    client.send_encoded(CLoginSuccess::packet_id(), |w| {
        CLoginSuccess {
            uuid,
            name: client.name.lock().expect("name").clone(),
            strict_error_handling: true,
        }
        .encode(w);
    })?;
    Ok(())
}

/// 配置阶段：注册表/特性/标签。
/// 配置阶段第一步：启用特性 + 已知资源包列表。
///
/// 对照官方 `ServerConfigurationNetworkHandler#sendConfigurations`：
/// `CustomPayload(brand)`（可选）→ `FeaturesS2CPacket` → `SelectKnownPacksS2CPacket`，
/// 然后等待客户端回显 known packs，再下发注册表与标签。
fn send_config_intro(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    let features = crate::net::registry::feature_flags();
    client.send_encoded(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_ENABLED_FEATURES,
        |w| features.encode(w),
    )?;
    // 服务器没有资源包，known packs 列表为空 → 客户端回空列表 → 注册表全量 NBT。
    let packs = crate::net::registry::known_packs();
    client.send_encoded(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_KNOWN_PACKS,
        |w| {
            w.write_var_int(i32::try_from(packs.len()).expect("packs"));
            for pack in &packs {
                w.write_string(&pack.namespace);
                w.write_string(&pack.id);
                w.write_string(&pack.version);
            }
        },
    )?;
    let _ = server;
    Ok(())
}

/// 配置阶段第二步：注册表数据 + 更新标签 + 完成配置。
///
/// 对照官方 `SynchronizeRegistriesTask#syncRegistryAndTags` 与 `JoinWorldTask`：
/// 收到客户端 known packs 回显后发 `DynamicRegistriesS2CPacket` ×N + `SynchronizeTagsS2CPacket`，
/// 随后发 ReadyS2CPacket（finish config）。
fn send_config_data(client: &Arc<Client>) -> Result<(), NetError> {
    // 官方 RegistryLoader.SYNCED_REGISTRIES 共 11 个；wolf_variant / painting_variant
    // 标记 requiredNonEmpty，客户端校验非空，必须全部下发。
    for registry in crate::net::registry::synced_registries() {
        client.send_encoded(
            icbm_mc_767::packets::config_ids::CLIENTBOUND_REGISTRY_DATA,
            |w| registry.encode(w),
        )?;
    }
    let tags = crate::net::registry::update_tags();
    client.send_encoded(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_TAGS,
        |w| tags.encode(w),
    )?;
    client.send_encoded(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_FINISH_CONFIGURATION,
        |w| icbm_mc_767::packets::config::CFinishConfig.encode(w),
    )?;
    Ok(())
}

/// 进入 Play：加入游戏 → 区块加载 → 位置同步。
pub fn enter_play(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    send_join_sequence(server, client)?;

    // 注册玩家（此后 receiver 可转发区块/聊天）
    {
        let mut world = server.world.lock().expect("world lock");
        let _ = world
            .players
            .insert(*client.uuid.lock().expect("uuid"), Arc::clone(client));
    }

    // 区块加载（先发“区块即将到达”让客户端显示加载界面）
    client.send_encoded(CGameEvent::packet_id(), |w| {
        CGameEvent {
            event: CGameEvent::EVENT_INITIAL_CHUNKS_COMING,
            value: 0.0,
        }
        .encode(w);
    })?;
    crate::net::play::load_initial_chunks(server, client)?;

    // 位置与传送确认
    let position = *client.position.lock().expect("position");
    let teleport_id = client.teleport_id.fetch_add(1, Ordering::Relaxed);
    client.send_encoded(CPlayerPosition::packet_id(), |w| {
        CPlayerPosition {
            x: position.x,
            y: position.y,
            z: position.z,
            yaw: position.yaw,
            pitch: position.pitch,
            flags: 0,
            teleport_id,
        }
        .encode(w);
    })?;

    let name = client.name.lock().expect("name").clone();
    info!(name = %name, "player entered the world");
    Ok(())
}

/// 发送加入游戏及初始包（加入游戏/中心/半径/难度/能力/玩家信息/出生点/命令树）。
fn send_join_sequence(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    // 加入游戏
    let spawn_data = icbm_mc_767::packets::play::clientbound::PlayerSpawnData {
        dimension_type: icbm_mc_767::packets::play::dimension_type_ids::OVERWORLD,
        dimension_name: icbm_mc_767::packets::play::dimension_names::OVERWORLD.to_owned(),
        hashed_seed: 0,
        game_mode: client.gamemode.lock().expect("gamemode").id(),
        previous_gamemode: -1,
        is_debug: false,
        is_flat: true,
        death_dimension: None,
        portal_cooldown: 0,
    };
    let login = icbm_mc_767::packets::play::clientbound::CLogin {
        entity_id: 1,
        is_hardcore: false,
        dimension_names: vec![
            icbm_mc_767::packets::play::dimension_names::OVERWORLD.to_owned(),
            icbm_mc_767::packets::play::dimension_names::THE_NETHER.to_owned(),
            icbm_mc_767::packets::play::dimension_names::THE_END.to_owned(),
        ],
        max_players: server.config.max_players,
        view_distance: server.config.view_distance,
        simulation_distance: server.config.simulation_distance,
        reduced_debug_info: false,
        enable_respawn_screen: true,
        limited_crafting: false,
        spawn_data,
        enforces_secure_chat: false,
    };
    client.send_encoded(
        icbm_mc_767::packets::play::clientbound::CLogin::packet_id(),
        |w| login.encode(w),
    )?;

    let chunk_x = client.position.lock().expect("position").chunk_x();
    let chunk_z = client.position.lock().expect("position").chunk_z();
    client.send_encoded(CSetChunkCacheCenter::packet_id(), |w| {
        CSetChunkCacheCenter { chunk_x, chunk_z }.encode(w);
    })?;
    let radius = client.view_distance.load(Ordering::Relaxed);
    client.send_encoded(CSetChunkCacheRadius::packet_id(), |w| {
        CSetChunkCacheRadius { radius }.encode(w);
    })?;
    client.send_encoded(
        icbm_mc_767::packets::play_ids::CLIENTBOUND_CHANGE_DIFFICULTY,
        |w| {
            icbm_mc_767::packets::play::clientbound::CChangeDifficulty {
                difficulty: server.config.difficulty,
                locked: false,
            }
            .encode(w);
        },
    )?;

    // 能力（按游戏模式）
    send_abilities(client)?;

    // 玩家信息（加入 Tab）
    let latency = client.latency.load(Ordering::Relaxed);
    let gamemode = client.gamemode.lock().expect("gamemode").id();
    client.send_encoded(CPlayerInfoUpdate::packet_id(), |w| {
        CPlayerInfoUpdate {
            players: vec![PlayerInfoEntry {
                uuid: *client.uuid.lock().expect("uuid"),
                name: client.name.lock().expect("name").clone(),
                game_mode: i32::from(gamemode),
                listed: true,
                latency,
            }],
        }
        .encode(w);
    })?;

    // 出生点与命令树
    let spawn = server.world.lock().expect("world lock").spawn;
    client.send_encoded(CSetDefaultSpawnPosition::packet_id(), |w| {
        CSetDefaultSpawnPosition {
            position: spawn,
            angle: 0.0,
        }
        .encode(w);
    })?;
    client.send_encoded(icbm_mc_767::packets::play_ids::CLIENTBOUND_COMMANDS, |w| {
        icbm_mc_767::packets::play::clientbound::CCommands.encode(w);
    })?;
    Ok(())
}

/// 按游戏模式发送能力包。
fn send_abilities(client: &Arc<Client>) -> Result<(), NetError> {
    let gamemode = *client.gamemode.lock().expect("gamemode");
    let flags = match gamemode {
        GameMode::Creative => CPlayerAbilities::FLAG_ALLOW_FLYING | CPlayerAbilities::FLAG_CREATIVE,
        GameMode::Spectator => {
            CPlayerAbilities::FLAG_INVULNERABLE
                | CPlayerAbilities::FLAG_FLYING
                | CPlayerAbilities::FLAG_ALLOW_FLYING
                | CPlayerAbilities::FLAG_CREATIVE
        }
        _ => 0,
    };
    client.send_encoded(CPlayerAbilities::packet_id(), |w| {
        CPlayerAbilities {
            flags,
            flying_speed: 0.05,
            fov_modifier: 0.1,
        }
        .encode(w);
    })?;
    Ok(())
}

/// 断线清理。
fn on_disconnect(server: &Server, client: &Arc<Client>) {
    let mut world = server.world.lock().expect("world lock");
    if world
        .players
        .remove(&*client.uuid.lock().expect("uuid"))
        .is_some()
    {
        let name = client.name.lock().expect("name").clone();
        info!(name = %name, "player left");
        if let Some(pending) = &world.chunk_pending
            && pending.player == *client.uuid.lock().expect("uuid")
        {
            world.chunk_pending = None;
        }
        // 通知其他玩家移除 Tab 项
        let uuids = vec![*client.uuid.lock().expect("uuid")];
        for other in world.players.values() {
            let _ = other.send_encoded(
                icbm_mc_767::packets::play_ids::CLIENTBOUND_PLAYER_INFO_REMOVE,
                |w| {
                    icbm_mc_767::packets::play::clientbound::CPlayerInfoRemove {
                        uuids: uuids.clone(),
                    }
                    .encode(w);
                },
            );
        }
        drop(uuids);
    }
}

/// 离线模式 UUID：MD5("OfflinePlayer:" + name)，版本 3 / IETF 变体。
#[must_use]
pub fn offline_uuid(name: &str) -> Uuid {
    let digest = md5::compute(format!("OfflinePlayer:{name}").as_bytes());
    let mut bytes = digest.0;
    bytes[6] = (bytes[6] & 0x0F) | 0x30;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::offline_uuid;

    #[test]
    fn offline_uuid_is_stable() {
        let a = offline_uuid("steve");
        let b = offline_uuid("steve");
        assert_eq!(a, b);
        assert_ne!(a, offline_uuid("alex"));
        // 版本 3、IETF 变体
        assert_eq!(a.get_version_num(), 3);
        assert_eq!(a.get_variant(), uuid::Variant::RFC4122);
    }
}
