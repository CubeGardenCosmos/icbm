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
    /// C→R：区块请求。
    pub const CHUNK_REQUEST: u8 = 0x06;
    /// R→C：区块数据。
    pub const CHUNK_DATA: u8 = 0x07;
    /// C→R：统计请求。
    pub const REGION_STATS: u8 = 0x08;
    /// R→C：统计响应。
    pub const REGION_STATS_RESP: u8 = 0x09;
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
    pub fn connect(shm_name: &str, service_name: &str) -> Result<Self, IpcError> {
        let options = FrontendOptions {
            shm_name: shm_name.to_owned(),
            service_name: service_name.to_owned(),
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

/// `REGISTER_ACK` 载荷：`[u8 dim][i32 rx][i32 rz][u8 ok]`。
#[must_use]
pub fn encode_register_ack(dim: u8, rx: i32, rz: i32, ok: bool) -> Vec<u8> {
    let mut out = encode_register(dim, rx, rz, 0);
    out.push(u8::from(ok));
    out
}

/// 解析 `REGISTER_ACK` 载荷。
pub fn decode_register_ack(body: &[u8]) -> Result<(u8, i32, i32, bool), IpcError> {
    let (dim, rx, rz, _) = decode_register(body)?;
    let ok = body.get(13).is_some_and(|&b| b != 0);
    Ok((dim, rx, rz, ok))
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
        let body = encode_register_ack(DIM_NETHER, 1, 1, true);
        let (dim, rx, rz, ok) = decode_register_ack(&body).unwrap();
        assert_eq!((dim, rx, rz, ok), (DIM_NETHER, 1, 1, true));
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

    fn heartbeat_round_trip() {
        let body = encode_heartbeat(19.5, true, 7, 999);
        let (tps, sleeping, loaded, tick) = decode_heartbeat(&body).unwrap();
        assert_eq!((tps, sleeping, loaded, tick), (19.5, true, 7, 999));
    }

    fn region_id_mapping() {
        let id = RegionId::from_overworld_chunk(100, -50);
        assert_eq!((id.rx, id.rz), (3, -2));
        assert_eq!(id.service_name(), "icbm.region.o_3_-2");
        let nether = RegionId::from_nether_chunk(25, 7);
        assert_eq!((nether.rx, nether.rz), (6, 1));
    }
}
