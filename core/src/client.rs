//! 客户端连接：状态机（握手/状态/登录/配置/Play）与包处理。
//!
//! 每连接一个线程；`TcpStream` 读在连接线程独占，写经 `Mutex` 供
//! datarouter receiver 线程转发区块。

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use icbm_gameplay_minecraft::commands::GameMode;
use icbm_mc_767::ConnectionState;
use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::nbt::{Nbt, NbtCompound};
use icbm_mc_767::packets::decode_frame;
use icbm_mc_767::packets::handshake::SIntent;
use icbm_mc_767::packets::login::{
    CEncryptionRequest, CLoginDisconnect, CLoginSuccess, CSetCompression, SEncryptionResponse,
    SLoginAcknowledged, SLoginStart,
};
use icbm_mc_767::packets::play::clientbound::{
    CGameEvent, CPlayerAbilities, CPlayerInfoUpdate, CPlayerPosition, CSetChunkCacheCenter,
    CSetChunkCacheRadius, CSetDefaultSpawnPosition, PlayerInfoEntry,
};
use icbm_mc_767::packets::play_ids;
use crate::net::crypto::AesCfb8;
use icbm_mc_767::packets::status::{CPongResponse, CStatusResponse, SPingRequest, SStatusRequest};
use icbm_mc_767::packets::status_ids;
use icbm_region::{OVERWORLD_SURFACE_FEET_Y, RegionId};
use tracing::{debug, info, trace, warn};
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
    /// 移动速度（方块/秒，由移动包估算；ICBM 快照用）。
    pub speed: Mutex<f64>,
    /// 上次位置更新基准（速度估算）。
    last_move: Mutex<(Position, Instant)>,
    /// 已发给客户端的区块缓存中心（移动触发区块补充用）。
    pub chunk_center: Mutex<(i32, i32)>,
    /// 可视距离。
    pub view_distance: AtomicI32,
    /// 当前维度（0=主世界，1=地狱，2=末地）。
    pub dimension: AtomicU8,
    /// 客户端语言标签（`ClientInformation.locale`，如 `zh_cn`；进服打印，变更 debug）。
    pub locale: Mutex<String>,
    /// 主手手持物品（`give`/创造模式设置；放置方块用）。
    pub held_item: Mutex<Option<String>>,
    /// 是否存活（连接可继续）。
    pub alive: AtomicBool,
    /// 待确认的 keep alive。
    keep_alive: Mutex<Option<(i64, Instant)>>,
    /// 传送 ID 计数。
    teleport_id: AtomicI32,
    /// 延迟（毫秒）。
    pub latency: AtomicI32,
    /// ICBM 客户端已声明可接收 `icbm_client:state_snapshot`（配置阶段 register）。
    pub icbm_snapshot_ok: AtomicBool,
    /// ICBM 玩家订阅状态（`icbm_client:state_subscription` enabled）。
    pub icbm_subscribed: AtomicBool,
    /// ICBM 快照单调序号。
    pub icbm_sequence: AtomicU64,
    /// 最近发送的包（ID, 载荷长度）环形缓冲，断线时 dump 用于定位协议错位。
    tx_log: Mutex<std::collections::VecDeque<(i32, usize)>>,
    /// 登录加密共享密钥（启用后 `Some`；正版验证与 AES-CFB8 用）。
    encryption_key: Mutex<Option<[u8; 16]>>,
    /// 写侧 AES-CFB8 加密器（启用后持续使用，状态跨包保持）。
    write_cipher: Mutex<Option<AesCfb8>>,
    /// 登录 verify token（`CEncryptionRequest` 发出时生成，回包时比对）。
    verify_token: Mutex<Option<Vec<u8>>>,
    /// 上次传送门检测时间（节流用）。
    pub last_portal_check: Mutex<Instant>,
    /// 待消费的传送门检测结果（`BLOCK_QUERY_RESP` 写入；-1 = 无）。
    pub pending_portal_block: std::sync::atomic::AtomicI32,
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
                y: f64::from(OVERWORLD_SURFACE_FEET_Y),
                z: 0.0,
                yaw: 0.0,
                pitch: 0.0,
            }),
            speed: Mutex::new(0.0),
            last_move: Mutex::new((
                Position {
                    x: 0.0,
                    y: f64::from(OVERWORLD_SURFACE_FEET_Y),
                    z: 0.0,
                    yaw: 0.0,
                    pitch: 0.0,
                },
                Instant::now(),
            )),
            chunk_center: Mutex::new((0, 0)),
            view_distance: AtomicI32::new(8),
            dimension: AtomicU8::new(0),
            locale: Mutex::new(String::new()),
            held_item: Mutex::new(None),
            alive: AtomicBool::new(true),
            keep_alive: Mutex::new(None),
            teleport_id: AtomicI32::new(0),
            latency: AtomicI32::new(0),
            icbm_snapshot_ok: AtomicBool::new(false),
            icbm_subscribed: AtomicBool::new(false),
            icbm_sequence: AtomicU64::new(0),
            tx_log: Mutex::new(std::collections::VecDeque::new()),
            encryption_key: Mutex::new(None),
            write_cipher: Mutex::new(None),
            verify_token: Mutex::new(None),
            last_portal_check: Mutex::new(Instant::now()),
            pending_portal_block: std::sync::atomic::AtomicI32::new(-1),
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

    /// 发送一个完整包（自动帧编码与压缩；登录加密启用后经 AES-CFB8 写出）。
    pub fn send_packet(&self, packet_id: i32, payload: &[u8]) -> Result<(), NetError> {
        let frame =
            icbm_mc_767::packets::encode_frame(packet_id, payload, self.compression_threshold())?;
        let mut write = self.write.lock().expect("write lock");
        let mut cipher = self.write_cipher.lock().expect("write cipher");
        match cipher.as_mut() {
            Some(cipher) => {
                let mut encrypted = frame;
                cipher.encrypt(&mut encrypted);
                write.write_all(&encrypted)?;
            }
            None => write.write_all(&frame)?,
        }
        {
            let mut log = self.tx_log.lock().expect("tx log");
            log.push_back((packet_id, payload.len()));
            while log.len() > 256 {
                log.pop_front();
            }
        }
        Ok(())
    }

    /// 断线时输出最近发送的包序列（诊断协议错位）。
    fn dump_tx_log(&self) {
        let log = self.tx_log.lock().expect("tx log");
        if log.is_empty() {
            return;
        }
        let mut items: Vec<String> = log
            .iter()
            .map(|(id, len)| format!("0x{id:02X}({len})"))
            .collect();
        // 太长的序列折叠成首尾
        if items.len() > 48 {
            let head = items[..24].join(" ");
            let tail = items[items.len() - 24..].join(" ");
            items = vec![format!("{head} ... {tail}")];
        }
        trace!("tx log: {}", items.join(" "));
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
    ///
    /// 内容按官方 1.20.5+ 格式编码为 NBT 组件 `{text: ...}`（客户端
    /// `FriendlyByteBuf.readNbt` 解析），非裸 JSON 字符串——裸字符串会让客户端
    /// 把首字节当 NBT 标签 ID 而断线（`Invalid tag id`）。
    pub fn send_system_chat(&self, message: &str, overlay: bool) -> Result<(), NetError> {
        let mut component = NbtCompound::new();
        component.insert("text", Nbt::String(message.to_owned()));
        self.send_encoded(play_ids::CLIENTBOUND_SYSTEM_CHAT, |w| {
            w.write_nbt_unnamed(&Nbt::Compound(Box::new(component)))
                .expect("system chat component must encode");
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

    /// 更新客户端语言（`ClientInformation.locale`）。
    ///
    /// 首次设置（进服）静默记录；游戏中变更输出 DEBUG（默认日志级别 INFO
    /// 不显示，满足"服务器默认不输出变更，仅在玩家加入时输出一次"）。
    pub fn set_locale(&self, locale: &str) {
        let mut stored = self.locale.lock().expect("locale");
        if stored.as_str() == locale {
            return;
        }
        let name = self.name.lock().expect("name").clone();
        if stored.is_empty() {
            *stored = locale.to_owned();
            debug!(name = %name, locale, "player language set");
        } else {
            let previous: String = stored.clone();
            *stored = locale.to_owned();
            debug!(name = %name, previous = %previous, locale, "player language changed");
        }
    }

    /// 记录一次位置更新并估算移动速度（方块/秒，3D 距离/间隔，EMA 平滑）。
    ///
    /// 间隔超出 [20ms, 2s] 视为传送/停滞：只更新基准不改变速度。
    pub fn note_move(&self) {let now = Instant::now();        let position = *self.position.lock().expect("position");
        let mut last = self.last_move.lock().expect("last move");
        let dt = now.duration_since(last.1).as_secs_f64();
        if (0.02..=2.0).contains(&dt) {
            let distance = ((position.x - last.0.x).powi(2)
                + (position.y - last.0.y).powi(2)
                + (position.z - last.0.z).powi(2))
            .sqrt();
            let instant = (distance / dt).clamp(0.0, 100.0);
            let mut speed = self.speed.lock().expect("speed");
            *speed = if *speed <= 0.0 {
                instant
            } else {
                *speed * 0.5 + instant * 0.5
            };
        }
        *last = (position, now);
    }

    /// 重置速度基准（传送/进服后调用，避免把传送位移计入速度）。
    pub fn reset_move_baseline(&self) {
        let position = *self.position.lock().expect("position");
        *self.last_move.lock().expect("last move") = (position, Instant::now());
    }

    /// 分配传送 ID（`CPlayerPosition` 用）。
    #[must_use]
    pub fn next_teleport_id(&self) -> i32 {
        self.teleport_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 对端 IP（正版验证 `hasJoined&ip=` 参数）。
    #[must_use]
    pub fn peer_ip(&self) -> Option<String> {
        let write = self.write.lock().expect("write lock");
        write.peer_addr().ok().map(|addr| addr.ip().to_string())
    }

    /// 登录加密共享密钥（启用后 `Some`；`handle_connection` 读侧据此建立解密器）。
    #[must_use]
    pub fn encryption_key(&self) -> Option<[u8; 16]> {
        *self.encryption_key.lock().expect("encryption key")
    }
}

/// 处理单个连接（阻塞直到断开）。
#[allow(clippy::too_many_lines)] // 状态机循环：帧解码/keep alive/读侧加密集中于此
pub fn handle_connection(server: &Server, mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok();
    let _ = stream.set_nodelay(true);

    let mut buffer: Vec<u8> = Vec::new();
    let mut last_keep_alive = Instant::now();
    let mut keep_alive_id: i64 = 0;
    let mut client: Option<Arc<Client>> = None;
    // 读侧 AES-CFB8 解密器（登录加密启用后建立，状态跨包保持）
    let mut read_cipher: Option<AesCfb8> = None;

    loop {
        // 读一块
        let mut chunk = [0u8; 8192];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let mut data = chunk[..n].to_vec();
                if let Some(cipher) = read_cipher.as_mut() {
                    cipher.decrypt(&mut data);
                }
                buffer.extend_from_slice(&data);
            }
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

        // 登录加密已建立（handle_login_key 设置了 encryption_key）：
        // 在本轮循环开始后启用读侧解密器（一次性）。
        if read_cipher.is_none()
            && let Some(client_ref) = &client
            && let Some(key) = client_ref.encryption_key()
        {
            read_cipher = Some(AesCfb8::new(&key));
        }
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
            icbm_mc_767::packets::login_ids::SERVERBOUND_KEY => {
                let key = SEncryptionResponse::decode(&mut reader)?;
                handle_login_key(server, client, &key)?;
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
                client.set_locale(&info.locale);
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
                let msg = icbm_mc_767::packets::config::SPluginMessage::decode(&mut reader)?;
                if msg.channel == "minecraft:register" {
                    // 客户端声明可接收的 S2C 通道（NUL 分隔）
                    for channel in register_channels(&msg.data) {
                        if channel == "icbm_client:state_snapshot" {
                            client.icbm_snapshot_ok.store(true, Ordering::Relaxed);
                            debug!(
                                name = %client.name.lock().expect("name"),
                                "icbm client declared state_snapshot"
                            );
                        }
                    }
                }
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

/// 登录开始：正版模式发加密请求，离线模式直接成功。
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
    *client.name.lock().expect("name") = name;
    // 玩家名合法性与正版验证在 `handle_login_key` 完成（加密响应后）。
    match server.online_mode {
        crate::net::crypto::OnlineMode::Offline => {
            let uuid = offline_uuid(&client.name.lock().expect("name"));
            *client.uuid.lock().expect("uuid") = uuid;
            finish_login(server, client, uuid)?;
        }
        crate::net::crypto::OnlineMode::Online => {
            // 正版模式：发 LoginHello（空 server_id、RSA 公钥、随机 token）。
            let token: [u8; 4] = rand::random();
            *client.verify_token.lock().expect("verify token") = Some(token.to_vec());
            client.send_encoded(CEncryptionRequest::packet_id(), |w| {
                CEncryptionRequest {
                    server_id: String::new(),
                    public_key: server.key_store.public_key_der().to_vec(),
                    verify_token: token.to_vec(),
                    needs_authentication: true,
                }
                .encode(w);
            })?;
        }
    }
    Ok(())
}

/// 加密响应：解密共享密钥、校验 token、启用流加密，随后按模式完成登录。
fn handle_login_key(
    server: &Server,
    client: &Arc<Client>,
    key: &SEncryptionResponse,
) -> Result<(), NetError> {
    use crate::net::crypto::CryptoError as CE;
    // 解密共享密钥
    let secret = server
        .key_store
        .decrypt(&key.encrypted_secret)
        .map_err(|_| NetError::BadPacket("decrypt shared secret failed"))?;
    let secret: [u8; 16] = secret
        .as_slice()
        .try_into()
        .map_err(|_| NetError::BadPacket("shared secret length != 16"))?;
    // 校验 verify token
    let token = server
        .key_store
        .decrypt(&key.encrypted_token)
        .map_err(|_| NetError::BadPacket("decrypt verify token failed"))?;
    let expected = client.verify_token.lock().expect("verify token");
    if expected.as_deref() != Some(token.as_slice()) {
        client.kick("登录验证失败 (token mismatch)");
        return Ok(());
    }
    // 启用加密：写侧 cipher + 读侧由 handle_connection 在下一轮启用
    let cipher = AesCfb8::new(&secret);
    *client.write_cipher.lock().expect("write cipher") = Some(cipher);
    *client.encryption_key.lock().expect("encryption key") = Some(secret);
    let _ = CE::Decrypt; // 保持导入使用
    drop(expected);

    match server.online_mode {
        crate::net::crypto::OnlineMode::Offline => {
            // 仅加密（不验证 Mojang）：离线 UUID
            let name = client.name.lock().expect("name").clone();
            let uuid = offline_uuid(&name);
            *client.uuid.lock().expect("uuid") = uuid;
            finish_login(server, client, uuid)?;
        }
        crate::net::crypto::OnlineMode::Online => {
            // 正版验证：server_hash → Mojang session server
            let hash = server.key_store.server_hash(&secret);
            let name = client.name.lock().expect("name").clone();
            let ip = client.peer_ip();
            match crate::net::crypto::verify_session(&name, &hash, ip.as_deref()) {
                Ok(profile) => {
                    let uuid = profile.uuid().map_err(|_| {
                        NetError::BadPacket("auth profile missing uuid")
                    })?;
                    *client.uuid.lock().expect("uuid") = uuid;
                    // 若 Mojang 返回的 name 与请求一致（大小写可不同），采用 profile 名
                    if !profile.name.is_empty() {
                        *client.name.lock().expect("name") = profile.name;
                    }
                    finish_login(server, client, uuid)?;
                }
                Err(err) => {
                    let reason = match &err {
                        CE::UnverifiedUsername => "正版验证失败：用户名未验证".to_owned(),
                        CE::Banned => "正版验证失败：账号已被封禁".to_owned(),
                        CE::AuthRequest => "正版验证失败：认证服务器不可用".to_owned(),
                        other => format!("正版验证失败：{other}"),
                    };
                    tracing::warn!(name = %client.name.lock().expect("name"), %err, "online auth failed");
                    client.kick(&reason);
                }
            }
        }
    }
    Ok(())
}

/// 完成登录：压缩 → 登录成功（正版/离线共用）。
fn finish_login(server: &Server, client: &Arc<Client>, uuid: Uuid) -> Result<(), NetError> {
    let name = client.name.lock().expect("name").clone();
    // 白名单/封禁检查（登录名单：封禁拒绝、白名单启用时拒绝未列入者）
    {
        let lists = server.lists.lock().expect("lists");
        if let Err(reason) = lists.check_login(&uuid, &name) {
            client.kick(&reason);
            return Ok(());
        }
    }
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
            name,
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
    // 官方 `ServerConfigurationNetworkHandler#sendConfigurations` 最先发 brand：
    // CustomPayloadS2CPacket(BrandCustomPayload(modName)) → Features → SelectKnownPacks。
    // 客户端 F3 的 "Server brand" 依赖此包，缺失显示 ~~NULL~~。
    client.send_encoded(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        |w| {
            w.write_string("minecraft:brand");
            w.write_string("ICBM");
        },
    )?;
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
    // 出生游戏模式（配置缺省；`Client::new` 默认生存）
    if let Some(mode) = GameMode::from_id(server.config.default_gamemode) {
        *client.gamemode.lock().expect("gamemode") = mode;
    }
    send_join_sequence(server, client)?;

    // 服务器声明可接收 `icbm_client:state_subscription`（C2S 通道）。
    // 对照 ICBM Client 协议：服务端进入 Play 后用原版 `minecraft:register` 声明。
    //
    // 原版 `minecraft:register` payload 是 NUL 分隔的原始通道名序列（对照
    // `DinnerboneProtocolUtils.writeChannels` 与 master 分支
    // `SUBSCRIPTION_CHANNEL_REGISTRATION`），不得写成 VarInt 长度前缀字符串——
    // 那会把长度字节 `0x1E` 读进通道名导致客户端 `ResourceLocationException`。
    client.send_encoded(
        icbm_mc_767::packets::play_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        |w| {
            w.write_string("minecraft:register");
            w.write_slice(b"icbm_client:state_subscription\0");
        },
    )?;

    // 注册玩家（此后 receiver 可转发区块/聊天）
    {
        let mut world = server.world.lock().expect("world lock");
        let _ = world
            .players
            .insert(*client.uuid.lock().expect("uuid"), Arc::clone(client));
    }
    // 玩家所在 region（实体/区块转发用；先于区块加载设置，避免进服早期
    // region 发来的实体包被丢弃）。Y>400 虚拟区不关联任何 region（core 接管）。
    {
        let position = client.position.lock().expect("position");
        if position.y <= crate::net::play::VIRTUAL_ZONE_MIN_Y {
            let region = RegionId::from_overworld_chunk(position.chunk_x(), position.chunk_z());
            let mut world = server.world.lock().expect("world lock");
            let _ = world
                .player_regions
                .insert(*client.uuid.lock().expect("uuid"), region);
        }
    }
    // 位置与传送确认：先于区块加载发送，对照 vanilla `PlayerManager#onPlayerConnect`
    // 的 `requestTeleport`（位置包在 chunk 批次之前）。客户端本地玩家初始位置是
    // 默认共享出生点 (8,64,8)（`AbstractClientPlayer` 取 `Level.getSharedSpawnPos`）；
    // 若把位置延迟到区块批次结束后才同步，加载界面结束后会从高空坠入虚空。
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
    // 传送后重置速度基准，避免把进服位移计入速度
    client.reset_move_baseline();

    // 区块加载（先发“区块即将到达”让客户端显示加载界面）
    client.send_encoded(CGameEvent::packet_id(), |w| {
        CGameEvent {
            event: CGameEvent::EVENT_INITIAL_CHUNKS_COMING,
            value: 0.0,
        }
        .encode(w);
    })?;
    crate::net::play::load_initial_chunks(server, client)?;

    let name = client.name.lock().expect("name").clone();
    let uuid = *client.uuid.lock().expect("uuid");
    let position = *client.position.lock().expect("position");
    let dimension = client.dimension.load(Ordering::Relaxed);
    let dimension_name = match dimension {
        1 => "minecraft:the_nether",
        2 => "minecraft:the_end",
        _ => "minecraft:overworld",
    };
    let ip = client.peer_ip().unwrap_or_else(|| "unknown".to_owned());
    let locale = client.locale.lock().expect("locale").clone();
    let locale = if locale.is_empty() { "unknown".to_owned() } else { locale };
    info!(
        name = %name,
        %uuid,
        %ip,
        dimension = %dimension_name,
        locale = %locale,
        x = position.x,
        y = position.y,
        z = position.z,
        "player entered the world"
    );
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
    // 区块缓存半径 = 服务端可视距离（对照 vanilla `ChunkLoadDistanceS2CPacket`）；
    // 不得用客户端上报的可视距离，否则进服请求范围会被客户端渲染距离撑大。
    let radius = crate::net::play::effective_view_distance(server);
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
        crate::net::play::build_command_tree().encode(w);
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
    client.dump_tx_log();
    let mut world = server.world.lock().expect("world lock");
    if world
        .players
        .remove(&*client.uuid.lock().expect("uuid"))
        .is_some()
    {
        let name = client.name.lock().expect("name").clone();
        info!(name = %name, "player left");
        world
            .player_regions
            .remove(&*client.uuid.lock().expect("uuid"));
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

/// 解析原版 `minecraft:register` payload（NUL 分隔的通道名列表）。
///
/// 对照 `DinnerboneProtocolUtils.readChannels`（逐字节读至 NUL，缓冲末尾再结算）与
/// master 分支 `registered_channel_contains`：payload 是原始 UTF-8 通道名序列，
/// 通道间以 `NUL`(0x00) 分隔，无长度前缀；非法 UTF-8 片段按 master 语义忽略
/// （不授予任何能力）。
#[must_use]
pub fn register_channels(data: &[u8]) -> Vec<String> {
    data.split(|&b| b == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{offline_uuid, register_channels};

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

    #[test]
    fn register_payload_is_nul_separated_raw_bytes() {
        // 对照 `DinnerboneProtocolUtils.readChannels` 与 master 分支
        // `registered_channel_contains`：payload 是 NUL 分隔的原始通道名，无长度前缀。
        assert_eq!(
            register_channels(b"minecraft:brand\0icbm_client:state_snapshot\0"),
            vec![
                "minecraft:brand".to_owned(),
                "icbm_client:state_snapshot".to_owned()
            ]
        );
    }

    #[test]
    fn register_payload_rejects_length_prefix_and_bad_utf8() {
        // 旧实现把通道名写成 VarInt 长度前缀字符串：`0x1E`(30) 是长度字节，
        // NeoForge 客户端会把它读进通道名导致 `ResourceLocationException`，
        // 解析结果不得等于原通道名。
        let channels = register_channels(b"\x1Eicbm_client:state_subscription");
        assert!(
            !channels
                .iter()
                .any(|ch| ch == "icbm_client:state_subscription"),
            "length-prefixed channel must not match: {channels:?}"
        );
        // 非法 UTF-8 片段不授予能力（master 语义）。
        let channels = register_channels(b"\xff\0icbm_client:state_snapshot\0");
        assert!(channels.iter().any(|ch| ch == "icbm_client:state_snapshot"));
    }
}
