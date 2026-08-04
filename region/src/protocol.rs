//! core ↔ region 二进制 IPC 协议（`docs/ipc.md` 的实现）。
//!
//! 消息经 datarouter `MSG_CONTROL`（0x8B）传输，body 为紧凑小端帧：
//! `[u8 kind][载荷]`。不使用业务 TLV/JSON 上下文。

use std::fmt;

use blinkcms_datarouter::frontend::{Frontend, FrontendOptions};
use blinkcms_datarouter::protocol::{self, MSG_CONTROL, Message};
use uuid::Uuid;

/// core 服务名。
pub const CORE_SERVICE: &str = "icbm.core";

/// 维度字节（IPC）。
pub const DIM_OVERWORLD: u8 = 0;
/// 地狱维度字节。
pub const DIM_NETHER: u8 = 1;
/// 末地维度字节。
pub const DIM_END: u8 = 2;

/// 帧 kind。
pub mod kind {
    /// R→C：注册。
    pub const REGION_REGISTER: u8 = 0x01;
    /// C→R：注册确认。
    pub const REGISTER_ACK: u8 = 0x81;
    /// R→C：心跳。
    pub const REGION_HEARTBEAT: u8 = 0x02;
    /// C→R：卸载。
    pub const REGION_UNLOAD: u8 = 0x03;
    /// C→R：玩家进入。
    pub const PLAYER_ENTER: u8 = 0x04;
    /// C→R：玩家离开。
    pub const PLAYER_LEAVE: u8 = 0x05;
    /// C→R：玩家移动（`[uuid 16B][f64 x][f64 y][f64 z]`；供敌对生物追踪）。
    pub const PLAYER_MOVE: u8 = 0x0F;
    /// C→R：区块请求。
    pub const CHUNK_REQUEST: u8 = 0x06;
    /// R→C：区块数据。
    pub const CHUNK_DATA: u8 = 0x07;
    /// C→R：统计请求。
    pub const REGION_STATS: u8 = 0x08;
    /// R→C：统计响应。
    pub const REGION_STATS_RESP: u8 = 0x09;
    /// R→C：实体包（AddEntity/MoveEntityPos 等，core 转发给区域玩家）。
    pub const ENTITY_DATA: u8 = 0x0A;
    /// C→R：方块修改（`setblock`/`fill`：`[u8 dim][i32 x][i32 y][i32 z][i32 state_id]`）。
    pub const BLOCK_UPDATE: u8 = 0x0B;
    /// R→C：玩家进入传送门（`[uuid 16B][u8 dim][i32 x][i32 y][i32 z]`）。
    pub const PORTAL_ENTER: u8 = 0x0C;
    /// C→R：方块查询（`[u8 dim][i32 x][i32 y][i32 z]` → `BLOCK_QUERY_RESP`）。
    pub const BLOCK_QUERY: u8 = 0x0D;
    /// R→C：方块查询响应（`[u8 dim][i32 x][i32 y][i32 z][i32 state_id]`）。
    pub const BLOCK_QUERY_RESP: u8 = 0x8D;
    /// C→R：游戏规则变更（`[u8 名长][名][u8 值长][值]`）。
    pub const GAMERULE_UPDATE: u8 = 0x0E;
    /// C→R：世界时间设置（`[i64 tick][u8 add]`；`/time` 命令用）。
    pub const TIME_UPDATE: u8 = 0x10;
    /// C→R：方块 NBT 读取（`[u8 dim][i32 x][i32 y][i32 z]` → `BLOCK_NBT_RESP`）。
    pub const BLOCK_NBT_GET: u8 = 0x11;
    /// R→C：方块 NBT 响应（`[u8 dim][i32 x][i32 y][i32 z][u8 has][NBT]`）。
    pub const BLOCK_NBT_RESP: u8 = 0x91;
    /// C→R：方块 NBT 写入（`[u8 dim][i32 x][i32 y][i32 z][NBT]`；空 NBT 清除）。
    pub const BLOCK_NBT_SET: u8 = 0x12;
    /// R→C：执行命令（`[u8 长][命令]`；命令方块触发，core 以控制台身份执行）。
    pub const COMMAND_EXECUTE: u8 = 0x13;
    /// C→R：天气状态同步（`[u8 raining][u8 thundering][f32 rain_gradient][f32 thunder_gradient]`）。
    pub const WEATHER_UPDATE: u8 = 0x15;
    /// C→R：地表高度查询（`[u8 dim][i32 x][i32 z]` → `SURFACE_RESP`；出生点适配用）。
    pub const SURFACE_QUERY: u8 = 0x16;
    /// R→C：地表高度响应（`[i32 y]` = 最高非空气方块 y+1）。
    pub const SURFACE_RESP: u8 = 0x96;
    /// C→R：方块射线检测（`[f64 ox][f64 oy][f64 oz][f64 dx][f64 dy][f64 dz][f64 max]`）。
    pub const BLOCK_RAYCAST: u8 = 0x14;
    /// R→C：射线命中响应（`[u8 hit][f64 x][f64 y][f64 z]`；未命中 hit=0）。
    pub const BLOCK_RAYCAST_RESP: u8 = 0x94;
    /// C→R：维度切换目的端生成传送门（`[u8 dim][i32 x][i32 y][i32 z]`；
    /// 玩家落点，region 在其周边建 4×5 框架并填充传送门方块）。
    pub const PORTAL_CREATE: u8 = 0x17;
    /// W→C：worker 注册（`[u32 pid][u8 cpu]`；core 记录 worker 入池）。
    pub const WORKER_REGISTER: u8 = 0x18;
    /// C→W：分配 region（`[i32 rx][i32 rz]`；worker 内启动该 region 模拟线程）。
    pub const WORKER_ASSIGN: u8 = 0x19;
    /// C→W：释放 region（`[i32 rx][i32 rz]`；worker 内停止并移除该 region）。
    pub const WORKER_RELEASE: u8 = 0x1A;
}

/// IPC 错误。
#[derive(Debug)]
pub enum IpcError {
    /// datarouter 返回码。
    Datarouter(i32),
    /// 协议解析失败。
    Protocol(&'static str),
    /// 载荷长度不符。
    Length(&'static str, usize),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Datarouter(code) => write!(f, "datarouter error code {code}"),
            Self::Protocol(msg) => write!(f, "protocol: {msg}"),
            Self::Length(field, len) => write!(f, "invalid length for {field}: {len}"),
        }
    }
}

impl std::error::Error for IpcError {}

/// 收到的 IPC 消息。
#[derive(Debug, Clone)]
pub struct IpcMessage {
    /// 来源服务名。
    pub source: String,
    /// 帧 kind。
    pub kind: u8,
    /// 载荷。
    pub body: Vec<u8>,
}

/// datarouter Frontend 薄封装。
pub struct Ipc {
    frontend: Frontend,
}

impl Ipc {
    /// 连接 datarouter 并注册服务名。
    ///
    /// `auth_token`：switch 以 `--token` 启动时的共享密钥（core 通过环境变量
    /// `ICBM_DATAROUTER_TOKEN` 传给 region 子进程）；未启用鉴权时传 `None`。
    pub fn connect(
        shm_name: &str,
        service_name: &str,
        auth_token: Option<String>,
    ) -> Result<Self, IpcError> {
        let options = FrontendOptions {
            shm_name: shm_name.to_owned(),
            service_name: service_name.to_owned(),
            auth_token,
            ..FrontendOptions::default()
        };
        let frontend = Frontend::connect(options).map_err(IpcError::Datarouter)?;
        Ok(Self { frontend })
    }

    /// 发送 `MSG_CONTROL` 帧（`var_data` 留空；kind 为 body 首字节）。
    pub fn send(&self, target: &str, kind: u8, body: &[u8]) -> Result<(), IpcError> {
        let mut frame = Vec::with_capacity(1 + body.len());
        frame.push(kind);
        frame.extend_from_slice(body);
        let mut message = Message::new(MSG_CONTROL);
        message.body = frame;
        let packed = protocol::pack(&message).map_err(IpcError::Protocol)?;
        self.frontend
            .send(target, &packed, 5000)
            .map_err(IpcError::Datarouter)
    }

    /// 发送并对暂时性满/忙（BUSY/FULL/AGAIN）短退避重试，避免大块
    /// 数据突发时（如 81 区块一次性下发超过 channel 容量）region 崩溃。
    pub fn send_retry(&self, target: &str, kind: u8, body: &[u8]) -> Result<(), IpcError> {
        let mut attempts = 0;
        loop {
            match self.send(target, kind, body) {
                Ok(()) => return Ok(()),
                Err(IpcError::Datarouter(code)) if matches!(code, 3 | 4 | 6) && attempts < 30 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(err) => return Err(err),
            }
        }
    }

    /// 接收一帧（超时返回 `Ok(None)`）。
    pub fn recv(&self, timeout_ms: u32) -> Result<Option<IpcMessage>, IpcError> {
        let message = match self.frontend.recv(timeout_ms) {
            Ok(message) => message,
            Err(code) if code == blinkcms_datarouter::DATAROUTER_ERR_TIMEOUT => return Ok(None),
            Err(code) => return Err(IpcError::Datarouter(code)),
        };
        let kind = message.message.body.first().copied().unwrap_or(0);
        let body = message.message.body.get(1..).unwrap_or_default().to_vec();
        Ok(Some(IpcMessage {
            source: message.source,
            kind,
            body,
        }))
    }
}

// ── 帧编解码（小端） ──────────────────────────────────────────────────────

/// `REGION_REGISTER` 载荷：`[u8 dim][i32 rx][i32 rz][u32 pid]`。
#[must_use]
pub fn encode_register(dim: u8, rx: i32, rz: i32, pid: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(13);
    out.push(dim);
    out.extend_from_slice(&rx.to_le_bytes());
    out.extend_from_slice(&rz.to_le_bytes());
    out.extend_from_slice(&pid.to_le_bytes());
    out
}

/// 解析 `REGION_REGISTER` 载荷。
pub fn decode_register(body: &[u8]) -> Result<(u8, i32, i32, u32), IpcError> {
    if body.len() < 13 {
        return Err(IpcError::Length("register", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        u32::from_le_bytes(body[9..13].try_into().expect("slice")),
    ))
}

/// `REGISTER_ACK` 载荷：`[u8 dim][i32 rx][i32 rz][u8 ok][i32 entity_id_base]`。
///
/// `entity_id_base` 为该 region 的实体 ID 基址（core 分配，区域内实体 ID 取
/// `[base, base+999]`；卸载时 core 据此发 `REMOVE_ENTITIES` 清理客户端实体）。
#[must_use]
pub fn encode_register_ack(dim: u8, rx: i32, rz: i32, ok: bool, entity_id_base: i32) -> Vec<u8> {
    let mut out = encode_register(dim, rx, rz, 0);
    out.push(u8::from(ok));
    out.extend_from_slice(&entity_id_base.to_le_bytes());
    out
}

/// 解析 `REGISTER_ACK` 载荷。
///
/// 旧版（无 `entity_id_base`，仅 14 字节）仍可解析，`entity_id_base` 视为 `None`。
pub fn decode_register_ack(body: &[u8]) -> Result<(u8, i32, i32, bool, Option<i32>), IpcError> {
    let (dim, rx, rz, _) = decode_register(body)?;
    let ok = body.get(13).is_some_and(|&b| b != 0);
    let entity_id_base = body
        .get(14..18)
        .map(|bytes| i32::from_le_bytes(bytes.try_into().expect("slice")));
    Ok((dim, rx, rz, ok, entity_id_base))
}

/// `REGION_HEARTBEAT` 载荷：`[f32 tps][u8 sleeping][u32 loaded][u64 tick]`。
#[must_use]
pub fn encode_heartbeat(tps: f32, sleeping: bool, loaded_chunks: u32, tick: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(17);
    out.extend_from_slice(&tps.to_le_bytes());
    out.push(u8::from(sleeping));
    out.extend_from_slice(&loaded_chunks.to_le_bytes());
    out.extend_from_slice(&tick.to_le_bytes());
    out
}

/// 解析 `REGION_HEARTBEAT` 载荷。
pub fn decode_heartbeat(body: &[u8]) -> Result<(f32, bool, u32, u64), IpcError> {
    if body.len() < 17 {
        return Err(IpcError::Length("heartbeat", body.len()));
    }
    Ok((
        f32::from_le_bytes(body[0..4].try_into().expect("slice")),
        body[4] != 0,
        u32::from_le_bytes(body[5..9].try_into().expect("slice")),
        u64::from_le_bytes(body[9..17].try_into().expect("slice")),
    ))
}

/// `REGION_UNLOAD` / `CHUNK_REQUEST` 的维度参数由服务侧直接构造。
#[must_use]
pub fn encode_unload(dim: u8, rx: i32, rz: i32) -> Vec<u8> {
    encode_register(dim, rx, rz, 0)
}

/// `PLAYER_ENTER` 载荷：`[uuid 16B][f64 x][f64 y][f64 z][f32 yaw][f32 pitch]`。
#[must_use]
pub fn encode_player_enter(uuid: &Uuid, pos: [f64; 3], rot: [f32; 2]) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(uuid.as_bytes());
    for value in pos {
        out.extend_from_slice(&value.to_le_bytes());
    }
    for value in rot {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// 解析 `PLAYER_ENTER` 载荷。
pub fn decode_player_enter(body: &[u8]) -> Result<(Uuid, [f64; 3], [f32; 2]), IpcError> {
    if body.len() < 48 {
        return Err(IpcError::Length("player enter", body.len()));
    }
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes.copy_from_slice(&body[0..16]);
    Ok((
        Uuid::from_bytes(uuid_bytes),
        [
            f64::from_le_bytes(body[16..24].try_into().expect("slice")),
            f64::from_le_bytes(body[24..32].try_into().expect("slice")),
            f64::from_le_bytes(body[32..40].try_into().expect("slice")),
        ],
        [
            f32::from_le_bytes(body[40..44].try_into().expect("slice")),
            f32::from_le_bytes(body[44..48].try_into().expect("slice")),
        ],
    ))
}

/// `PLAYER_LEAVE` 载荷：`[uuid 16B]`。
#[must_use]
pub fn encode_player_leave(uuid: &Uuid) -> Vec<u8> {
    uuid.as_bytes().to_vec()
}

/// 解析 `PLAYER_LEAVE` 载荷。
pub fn decode_player_leave(body: &[u8]) -> Result<Uuid, IpcError> {
    if body.len() < 16 {
        return Err(IpcError::Length("player leave", body.len()));
    }
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes.copy_from_slice(&body[0..16]);
    Ok(Uuid::from_bytes(uuid_bytes))
}

/// `PLAYER_MOVE` 载荷：`[uuid 16B][f64 x][f64 y][f64 z]`。
#[must_use]
pub fn encode_player_move(uuid: &Uuid, x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(uuid.as_bytes());
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out
}

/// 解析 `PLAYER_MOVE` 载荷。
pub fn decode_player_move(body: &[u8]) -> Result<(Uuid, f64, f64, f64), IpcError> {
    if body.len() < 40 {
        return Err(IpcError::Length("player move", body.len()));
    }
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes.copy_from_slice(&body[0..16]);
    Ok((
        Uuid::from_bytes(uuid_bytes),
        f64::from_le_bytes(body[16..24].try_into().expect("slice")),
        f64::from_le_bytes(body[24..32].try_into().expect("slice")),
        f64::from_le_bytes(body[32..40].try_into().expect("slice")),
    ))
}

/// `CHUNK_REQUEST` 载荷：`[u8 dim][i32 cx][i32 cz][i32 radius]`。
#[must_use]
pub fn encode_chunk_request(dim: u8, cx: i32, cz: i32, radius: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(13);
    out.push(dim);
    out.extend_from_slice(&cx.to_le_bytes());
    out.extend_from_slice(&cz.to_le_bytes());
    out.extend_from_slice(&radius.to_le_bytes());
    out
}

/// 解析 `CHUNK_REQUEST` 载荷。
pub fn decode_chunk_request(body: &[u8]) -> Result<(u8, i32, i32, i32), IpcError> {
    if body.len() < 13 {
        return Err(IpcError::Length("chunk request", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        i32::from_le_bytes(body[9..13].try_into().expect("slice")),
    ))
}

/// `CHUNK_DATA` 载荷：`[u8 dim][i32 cx][i32 cz][u32 len][网络区块数据]`。
#[must_use]
pub fn encode_chunk_data(dim: u8, cx: i32, cz: i32, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(13 + data.len());
    out.push(dim);
    out.extend_from_slice(&cx.to_le_bytes());
    out.extend_from_slice(&cz.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(data.len())
            .expect("chunk data exceeds u32")
            .to_le_bytes(),
    );
    out.extend_from_slice(data);
    out
}

/// 解析 `CHUNK_DATA` 载荷。
pub fn decode_chunk_data(body: &[u8]) -> Result<(u8, i32, i32, &[u8]), IpcError> {
    if body.len() < 13 {
        return Err(IpcError::Length("chunk data", body.len()));
    }
    let len = u32::from_le_bytes(body[9..13].try_into().expect("slice")) as usize;
    if body.len() < 13 + len {
        return Err(IpcError::Length("chunk data payload", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        &body[13..13 + len],
    ))
}

/// `REGION_STATS_RESP` 载荷：`[f32 tps][f32 avg_tick_ms][u32 ticks][u8 sleeping][u32 loaded]`。
#[must_use]
pub fn encode_stats(
    tps: f32,
    avg_tick_ms: f32,
    tick_count: u32,
    sleeping: bool,
    loaded: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(17);
    out.extend_from_slice(&tps.to_le_bytes());
    out.extend_from_slice(&avg_tick_ms.to_le_bytes());
    out.extend_from_slice(&tick_count.to_le_bytes());
    out.push(u8::from(sleeping));
    out.extend_from_slice(&loaded.to_le_bytes());
    out
}

/// 解析 `REGION_STATS`_RESP 载荷。
pub fn decode_stats(body: &[u8]) -> Result<(f32, f32, u32, bool, u32), IpcError> {
    if body.len() < 17 {
        return Err(IpcError::Length("stats", body.len()));
    }
    Ok((
        f32::from_le_bytes(body[0..4].try_into().expect("slice")),
        f32::from_le_bytes(body[4..8].try_into().expect("slice")),
        u32::from_le_bytes(body[8..12].try_into().expect("slice")),
        body[12] != 0,
        u32::from_le_bytes(body[13..17].try_into().expect("slice")),
    ))
}

/// `ENTITY_DATA` 载荷：`[u8 dim][i32 rx][i32 rz][i32 packet_id][clientbound 载荷]`。
///
/// core 收到后把 `(packet_id, 载荷)` 转发给位于 `(rx, rz)` 区域的在线玩家。
#[must_use]
pub fn encode_entity_data(dim: u8, rx: i32, rz: i32, packet_id: i32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(17 + payload.len());
    out.push(dim);
    out.extend_from_slice(&rx.to_le_bytes());
    out.extend_from_slice(&rz.to_le_bytes());
    out.extend_from_slice(&packet_id.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// 解析 `ENTITY_DATA` 载荷。
pub fn decode_entity_data(body: &[u8]) -> Result<(u8, i32, i32, i32, &[u8]), IpcError> {
    if body.len() < 17 {
        return Err(IpcError::Length("entity data", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        i32::from_le_bytes(body[9..13].try_into().expect("slice")),
        &body[13..],
    ))
}

/// `BLOCK_UPDATE` 载荷：`[u8 dim][i32 x][i32 y][i32 z][i32 state_id]`。
#[must_use]
pub fn encode_block_update(dim: u8, x: i32, y: i32, z: i32, state_id: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(21);
    out.push(dim);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out.extend_from_slice(&state_id.to_le_bytes());
    out
}

/// 解析 `BLOCK_UPDATE` 载荷。
pub fn decode_block_update(body: &[u8]) -> Result<(u8, i32, i32, i32, i32), IpcError> {
    if body.len() < 17 {
        return Err(IpcError::Length("block update", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        i32::from_le_bytes(body[9..13].try_into().expect("slice")),
        i32::from_le_bytes(body[13..17].try_into().expect("slice")),
    ))
}

/// `PORTAL_ENTER` 载荷：`[uuid 16B][u8 dim][i32 x][i32 y][i32 z]`。
#[must_use]
pub fn encode_portal_enter(uuid: &Uuid, dim: u8, x: i32, y: i32, z: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(29);
    out.extend_from_slice(uuid.as_bytes());
    out.push(dim);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out
}

/// 解析 `PORTAL_ENTER` 载荷。
pub fn decode_portal_enter(body: &[u8]) -> Result<(Uuid, u8, i32, i32, i32), IpcError> {
    if body.len() < 29 {
        return Err(IpcError::Length("portal enter", body.len()));
    }
    let mut uuid_bytes = [0u8; 16];
    uuid_bytes.copy_from_slice(&body[0..16]);
    Ok((
        Uuid::from_bytes(uuid_bytes),
        body[16],
        i32::from_le_bytes(body[17..21].try_into().expect("slice")),
        i32::from_le_bytes(body[21..25].try_into().expect("slice")),
        i32::from_le_bytes(body[25..29].try_into().expect("slice")),
    ))
}

/// `PORTAL_CREATE` 载荷：`[u8 dim][i32 x][i32 y][i32 z]`（玩家落点）。
#[must_use]
pub fn encode_portal_create(dim: u8, x: i32, y: i32, z: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(13);
    out.push(dim);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out
}

/// 解析 `PORTAL_CREATE` 载荷。
pub fn decode_portal_create(body: &[u8]) -> Result<(u8, i32, i32, i32), IpcError> {
    if body.len() < 13 {
        return Err(IpcError::Length("portal create", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        i32::from_le_bytes(body[9..13].try_into().expect("slice")),
    ))
}

/// `BLOCK_QUERY` 载荷：`[u8 dim][i32 x][i32 y][i32 z]`。
#[must_use]
pub fn encode_block_query(dim: u8, x: i32, y: i32, z: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(13);
    out.push(dim);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out
}

/// 解析 `BLOCK_QUERY` 载荷。
pub fn decode_block_query(body: &[u8]) -> Result<(u8, i32, i32, i32), IpcError> {
    if body.len() < 13 {
        return Err(IpcError::Length("block query", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
        i32::from_le_bytes(body[9..13].try_into().expect("slice")),
    ))
}

/// `BLOCK_QUERY_RESP` 载荷：`[u8 dim][i32 x][i32 y][i32 z][i32 state_id]`。
#[must_use]
pub fn encode_block_query_resp(dim: u8, x: i32, y: i32, z: i32, state_id: i32) -> Vec<u8> {
    let mut out = encode_block_query(dim, x, y, z);
    out.extend_from_slice(&state_id.to_le_bytes());
    out
}

/// 解析 `BLOCK_QUERY_RESP` 载荷。
pub fn decode_block_query_resp(body: &[u8]) -> Result<(u8, i32, i32, i32, i32), IpcError> {
    let (dim, x, y, z) = decode_block_query(body)?;
    if body.len() < 17 {
        return Err(IpcError::Length("block query resp", body.len()));
    }
    Ok((
        dim,
        x,
        y,
        z,
        i32::from_le_bytes(body[13..17].try_into().expect("slice")),
    ))
}

/// `GAMERULE_UPDATE` 载荷：`[u8 名长][名][u8 值长][值]`。
#[must_use]
pub fn encode_gamerule_update(name: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + name.len() + value.len());
    out.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(name.as_bytes());
    out.push(u8::try_from(value.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(value.as_bytes());
    out
}

/// 解析 `GAMERULE_UPDATE` 载荷。
pub fn decode_gamerule_update(body: &[u8]) -> Result<(String, String), IpcError> {
    if body.len() < 2 {
        return Err(IpcError::Length("gamerule update", body.len()));
    }
    let name_len = usize::from(body[0]);
    if body.len() < 1 + name_len + 1 {
        return Err(IpcError::Length("gamerule update", body.len()));
    }
    let value_len = usize::from(body[1 + name_len]);
    if body.len() < 2 + name_len + value_len {
        return Err(IpcError::Length("gamerule update", body.len()));
    }
    let name = String::from_utf8_lossy(&body[1..=name_len]).into_owned();
    let value = String::from_utf8_lossy(&body[2 + name_len..2 + name_len + value_len]).into_owned();
    Ok((name, value))
}

/// `TIME_UPDATE` 载荷：`[i64 tick][u8 add]`（add=1 表示在 region 当前时间上累加）。
#[must_use]
pub fn encode_time_update(tick: i64, add: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    out.extend_from_slice(&tick.to_le_bytes());
    out.push(u8::from(add));
    out
}

/// 解析 `TIME_UPDATE` 载荷。
pub fn decode_time_update(body: &[u8]) -> Result<(i64, bool), IpcError> {
    if body.len() < 9 {
        return Err(IpcError::Length("time update", body.len()));
    }
    Ok((
        i64::from_le_bytes(body[0..8].try_into().expect("slice")),
        body[8] != 0,
    ))
}

/// `BLOCK_NBT_GET` 载荷：`[u8 dim][i32 x][i32 y][i32 z]`。
#[must_use]
pub fn encode_block_nbt_get(dim: u8, x: i32, y: i32, z: i32) -> Vec<u8> {
    encode_block_query(dim, x, y, z)
}

/// 解析 `BLOCK_NBT_GET` 载荷。
pub fn decode_block_nbt_get(body: &[u8]) -> Result<(u8, i32, i32, i32), IpcError> {
    decode_block_query(body)
}

/// `BLOCK_NBT_RESP` 载荷：`[u8 dim][i32 x][i32 y][i32 z][u8 has][NBT]`。
#[must_use]
pub fn encode_block_nbt_resp(
    dim: u8,
    x: i32,
    y: i32,
    z: i32,
    nbt: Option<&icbm_mc_767::nbt::Nbt>,
) -> Vec<u8> {
    let mut out = encode_block_nbt_get(dim, x, y, z);
    match nbt {
        Some(nbt) => {
            out.push(1);
            let mut writer = icbm_mc_767::codec::Writer::new();
            icbm_mc_767::nbt::encode_named(nbt, &mut writer).expect("block nbt must encode");
            out.extend_from_slice(&writer.into_bytes());
        }
        None => out.push(0),
    }
    out
}

/// 解析 `BLOCK_NBT_RESP` 载荷。
pub fn decode_block_nbt_resp(
    body: &[u8],
) -> Result<(u8, i32, i32, i32, Option<icbm_mc_767::nbt::Nbt>), IpcError> {
    let (dim, x, y, z) = decode_block_nbt_get(body)?;
    let has = body.get(13).is_some_and(|&b| b != 0);
    if !has {
        return Ok((dim, x, y, z, None));
    }
    let mut reader = icbm_mc_767::codec::Reader::new(&body[14..]);
    let nbt = icbm_mc_767::nbt::decode_named(&mut reader)
        .map_err(|_| IpcError::Protocol("block nbt parse"))?;
    Ok((dim, x, y, z, Some(nbt)))
}

/// `BLOCK_NBT_SET` 载荷：`[u8 dim][i32 x][i32 y][i32 z][NBT]`。
#[must_use]
pub fn encode_block_nbt_set(
    dim: u8,
    x: i32,
    y: i32,
    z: i32,
    nbt: &icbm_mc_767::nbt::Nbt,
) -> Vec<u8> {
    let mut out = encode_block_nbt_get(dim, x, y, z);
    let mut writer = icbm_mc_767::codec::Writer::new();
    icbm_mc_767::nbt::encode_named(nbt, &mut writer).expect("block nbt must encode");
    out.extend_from_slice(&writer.into_bytes());
    out
}

/// 解析 `BLOCK_NBT_SET` 载荷。
pub fn decode_block_nbt_set(
    body: &[u8],
) -> Result<(u8, i32, i32, i32, icbm_mc_767::nbt::Nbt), IpcError> {
    let (dim, x, y, z) = decode_block_nbt_get(body)?;
    let mut reader = icbm_mc_767::codec::Reader::new(&body[13..]);
    let nbt = icbm_mc_767::nbt::decode_named(&mut reader)
        .map_err(|_| IpcError::Protocol("block nbt set"))?;
    Ok((dim, x, y, z, nbt))
}

/// `COMMAND_EXECUTE` 载荷：`[u8 长][命令]`。
#[must_use]
pub fn encode_command_execute(command: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + command.len());
    out.push(u8::try_from(command.len()).unwrap_or(u8::MAX));
    out.extend_from_slice(command.as_bytes());
    out
}

/// 解析 `COMMAND_EXECUTE` 载荷。
pub fn decode_command_execute(body: &[u8]) -> Result<String, IpcError> {
    let len = usize::from(body.first().copied().unwrap_or(0));
    if body.len() < 1 + len {
        return Err(IpcError::Length("command execute", body.len()));
    }
    Ok(String::from_utf8_lossy(&body[1..=len]).into_owned())
}

/// `BLOCK_RAYCAST` 载荷：`[f64 ox][f64 oy][f64 oz][f64 dx][f64 dy][f64 dz][f64 max]`。
#[must_use]
pub fn encode_block_raycast(origin: [f64; 3], dir: [f64; 3], max: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(56);
    for v in origin.into_iter().chain(dir) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&max.to_le_bytes());
    out
}

/// 解析 `BLOCK_RAYCAST` 载荷。
pub fn decode_block_raycast(body: &[u8]) -> Result<([f64; 3], [f64; 3], f64), IpcError> {
    if body.len() < 56 {
        return Err(IpcError::Length("block raycast", body.len()));
    }
    let mut origin = [0.0; 3];
    let mut dir = [0.0; 3];
    for (i, v) in origin.iter_mut().chain(dir.iter_mut()).enumerate() {
        let off = i * 8;
        *v = f64::from_le_bytes(body[off..off + 8].try_into().expect("slice"));
    }
    let max = f64::from_le_bytes(body[48..56].try_into().expect("slice"));
    Ok((origin, dir, max))
}

/// `BLOCK_RAYCAST_RESP` 载荷：`[u8 hit][f64 x][f64 y][f64 z]`（未命中时坐标无效）。
#[must_use]
pub fn encode_block_raycast_resp(hit: bool, x: f64, y: f64, z: f64) -> Vec<u8> {
    let mut out = Vec::with_capacity(25);
    out.push(u8::from(hit));
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&y.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out
}

/// 解析 `BLOCK_RAYCAST_RESP` 载荷。
pub fn decode_block_raycast_resp(body: &[u8]) -> Result<(bool, f64, f64, f64), IpcError> {
    if body.len() < 25 {
        return Err(IpcError::Length("block raycast resp", body.len()));
    }
    Ok((
        body[0] != 0,
        f64::from_le_bytes(body[1..9].try_into().expect("slice")),
        f64::from_le_bytes(body[9..17].try_into().expect("slice")),
        f64::from_le_bytes(body[17..25].try_into().expect("slice")),
    ))
}

/// `WEATHER_UPDATE` 载荷：`[u8 raining][u8 thundering][f32 rain_gradient][f32 thunder_gradient]`。
#[must_use]
pub fn encode_weather_update(raining: bool, thundering: bool, rain: f32, thunder: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    out.push(u8::from(raining));
    out.push(u8::from(thundering));
    out.extend_from_slice(&rain.to_le_bytes());
    out.extend_from_slice(&thunder.to_le_bytes());
    out
}

/// 解析 `WEATHER_UPDATE` 载荷。
pub fn decode_weather_update(body: &[u8]) -> Result<(bool, bool, f32, f32), IpcError> {
    if body.len() < 10 {
        return Err(IpcError::Length("weather update", body.len()));
    }
    Ok((
        body[0] != 0,
        body[1] != 0,
        f32::from_le_bytes(body[2..6].try_into().expect("slice")),
        f32::from_le_bytes(body[6..10].try_into().expect("slice")),
    ))
}

/// `SURFACE_QUERY` 载荷：`[u8 dim][i32 x][i32 z]`。
#[must_use]
pub fn encode_surface_query(dim: u8, x: i32, z: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(9);
    out.push(dim);
    out.extend_from_slice(&x.to_le_bytes());
    out.extend_from_slice(&z.to_le_bytes());
    out
}

/// 解析 `SURFACE_QUERY` 载荷。
pub fn decode_surface_query(body: &[u8]) -> Result<(u8, i32, i32), IpcError> {
    if body.len() < 9 {
        return Err(IpcError::Length("surface query", body.len()));
    }
    Ok((
        body[0],
        i32::from_le_bytes(body[1..5].try_into().expect("slice")),
        i32::from_le_bytes(body[5..9].try_into().expect("slice")),
    ))
}

/// `SURFACE_RESP` 载荷：`[i32 y]`（最高非空气方块 y + 1；整列为空时 = 世界最低 Y）。
#[must_use]
pub fn encode_surface_resp(y: i32) -> Vec<u8> {
    y.to_le_bytes().to_vec()
}

/// 解析 `SURFACE_RESP` 载荷。
pub fn decode_surface_resp(body: &[u8]) -> Result<i32, IpcError> {
    if body.len() < 4 {
        return Err(IpcError::Length("surface resp", body.len()));
    }
    Ok(i32::from_le_bytes(body[0..4].try_into().expect("slice")))
}

/// `WORKER_REGISTER` 载荷：`[u32 pid][u8 cpu]`。
#[must_use]
pub fn encode_worker_register(pid: u32, cpu: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.extend_from_slice(&pid.to_le_bytes());
    out.push(cpu);
    out
}

/// 解析 `WORKER_REGISTER` 载荷。
pub fn decode_worker_register(body: &[u8]) -> Result<(u32, u8), IpcError> {
    if body.len() < 5 {
        return Err(IpcError::Length("worker register", body.len()));
    }
    Ok((
        u32::from_le_bytes(body[0..4].try_into().expect("slice")),
        body[4],
    ))
}

/// `WORKER_ASSIGN` / `WORKER_RELEASE` 载荷：`[i32 rx][i32 rz]`。
#[must_use]
pub fn encode_worker_region(rx: i32, rz: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    out.extend_from_slice(&rx.to_le_bytes());
    out.extend_from_slice(&rz.to_le_bytes());
    out
}

/// 解析 `WORKER_ASSIGN` / `WORKER_RELEASE` 载荷。
pub fn decode_worker_region(body: &[u8]) -> Result<(i32, i32), IpcError> {
    if body.len() < 8 {
        return Err(IpcError::Length("worker region", body.len()));
    }
    Ok((
        i32::from_le_bytes(body[0..4].try_into().expect("slice")),
        i32::from_le_bytes(body[4..8].try_into().expect("slice")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RegionId;

    #[test]
    fn register_round_trip() {
        let body = encode_register(DIM_OVERWORLD, -3, 5, 0xDEAD_BEEF);
        let (dim, rx, rz, pid) = decode_register(&body).unwrap();
        assert_eq!((dim, rx, rz, pid), (DIM_OVERWORLD, -3, 5, 0xDEAD_BEEF));
    }

    #[test]
    fn register_ack_round_trip() {
        let body = encode_register_ack(DIM_NETHER, 1, 1, true, 2000);
        let (dim, rx, rz, ok, base) = decode_register_ack(&body).unwrap();
        assert_eq!(
            (dim, rx, rz, ok, base),
            (DIM_NETHER, 1, 1, true, Some(2000))
        );
        // 旧版 ACK（无 entity_id_base）仍可解析
        let legacy = encode_register_ack(DIM_OVERWORLD, 0, 0, true, 0);
        let (_, _, _, ok, base) = decode_register_ack(&legacy[..14]).unwrap();
        assert!(ok);
        assert_eq!(base, None);
    }

    #[test]
    fn chunk_request_round_trip() {
        let body = encode_chunk_request(DIM_OVERWORLD, 100, -50, 8);
        let (dim, cx, cz, radius) = decode_chunk_request(&body).unwrap();
        assert_eq!((dim, cx, cz, radius), (DIM_OVERWORLD, 100, -50, 8));
    }

    #[test]
    fn chunk_data_round_trip() {
        let payload = vec![1u8, 2, 3, 4];
        let body = encode_chunk_data(DIM_OVERWORLD, 0, 0, &payload);
        let (dim, cx, cz, data) = decode_chunk_data(&body).unwrap();
        assert_eq!((dim, cx, cz), (DIM_OVERWORLD, 0, 0));
        assert_eq!(data, &payload[..]);
    }

    #[test]
    fn stats_round_trip() {
        let body = encode_stats(19.8, 50.5, 12345, true, 42);
        let (tps, avg, ticks, sleeping, loaded) = decode_stats(&body).unwrap();
        assert_eq!(
            (tps, avg, ticks, sleeping, loaded),
            (19.8, 50.5, 12345, true, 42)
        );
    }

    #[test]
    fn entity_data_round_trip() {
        let payload = [0x2Eu8, 0x01, 0x00, 0x02];
        let body = encode_entity_data(DIM_OVERWORLD, 3, -2, 0x2E, &payload);
        let (dim, rx, rz, packet_id, data) = decode_entity_data(&body).unwrap();
        assert_eq!((dim, rx, rz, packet_id), (DIM_OVERWORLD, 3, -2, 0x2E));
        assert_eq!(data, &payload[..]);
    }

    #[test]
    fn block_update_round_trip() {
        let body = encode_block_update(DIM_OVERWORLD, 12, 64, -8, 9);
        let (dim, x, y, z, state) = decode_block_update(&body).unwrap();
        assert_eq!((dim, x, y, z, state), (DIM_OVERWORLD, 12, 64, -8, 9));
    }

    #[test]
    fn portal_enter_round_trip() {
        let uuid = Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        let body = encode_portal_enter(&uuid, DIM_NETHER, 3, 64, -8);
        let (decoded, dim, x, y, z) = decode_portal_enter(&body).unwrap();
        assert_eq!(decoded, uuid);
        assert_eq!((dim, x, y, z), (DIM_NETHER, 3, 64, -8));
    }

    #[test]
    fn portal_create_round_trip() {
        let body = encode_portal_create(DIM_OVERWORLD, -12, 64, 300);
        let (dim, x, y, z) = decode_portal_create(&body).unwrap();
        assert_eq!((dim, x, y, z), (DIM_OVERWORLD, -12, 64, 300));
    }

    #[test]
    fn gamerule_update_round_trip() {
        let body = encode_gamerule_update("doDaylightCycle", "false");
        let (name, value) = decode_gamerule_update(&body).unwrap();
        assert_eq!(name, "doDaylightCycle");
        assert_eq!(value, "false");
        // 空名/空值可解析
        let body = encode_gamerule_update("", "");
        let (name, value) = decode_gamerule_update(&body).unwrap();
        assert_eq!((name.as_str(), value.as_str()), ("", ""));
    }

    #[test]
    fn player_enter_leave_round_trip() {
        let uuid = Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        let body = encode_player_enter(&uuid, [1.5, 64.0, -3.25], [90.0, 45.0]);
        let (decoded_uuid, pos, rot) = decode_player_enter(&body).unwrap();
        assert_eq!(decoded_uuid, uuid);
        assert_eq!(pos, [1.5, 64.0, -3.25]);
        assert_eq!(rot, [90.0, 45.0]);
        let leave = encode_player_leave(&uuid);
        assert_eq!(decode_player_leave(&leave).unwrap(), uuid);
    }

    #[test]
    fn heartbeat_round_trip() {
        let body = encode_heartbeat(19.5, true, 7, 999);
        let (tps, sleeping, loaded, tick) = decode_heartbeat(&body).unwrap();
        assert_eq!((tps, sleeping, loaded, tick), (19.5, true, 7, 999));
    }

    #[test]
    fn region_id_mapping() {
        let id = RegionId::from_overworld_chunk(100, -50);
        assert_eq!((id.rx, id.rz), (3, -2));
        assert_eq!(id.service_name(), "icbm.region.o_3_-2");
        let nether = RegionId::from_nether_chunk(25, 7);
        assert_eq!((nether.rx, nether.rz), (6, 1));
    }

    #[test]
    fn worker_messages_round_trip() {
        let reg = encode_worker_register(12345, 2);
        assert_eq!(decode_worker_register(&reg).unwrap(), (12345, 2));
        let assign = encode_worker_region(-3, 5);
        assert_eq!(decode_worker_region(&assign).unwrap(), (-3, 5));
        assert!(decode_worker_region(&assign[..4]).is_err());
    }
}
