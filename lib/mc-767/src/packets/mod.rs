//! 包 ID、帧编解码（可选 zlib 压缩）与各状态的最小包集。
//!
//! 帧格式（1.21.1）：
//! - 无压缩：`[VarInt 长度][VarInt 包ID][载荷]`
//! - 有压缩：`[VarInt 数据长度][VarInt 未压缩长度][zlib(包ID+载荷)]`，
//!   其中数据长度 0 表示未压缩（未压缩长度字段省略）。

pub mod config;
pub mod handshake;
pub mod login;
pub mod play;
pub mod status;

use std::fmt;

use crate::codec::{CodecError, Reader, Writer};

/// 握手 serverbound 包 ID。
pub mod handshake_ids {
    pub const INTENTION: i32 = 0x00;
}

/// 状态包 ID。
pub mod status_ids {
    pub const SERVERBOUND_REQUEST: i32 = 0x00;
    pub const SERVERBOUND_PING: i32 = 0x01;
    pub const CLIENTBOUND_RESPONSE: i32 = 0x00;
    pub const CLIENTBOUND_PONG: i32 = 0x01;
}

/// 登录包 ID。
pub mod login_ids {
    pub const SERVERBOUND_START: i32 = 0x00;
    pub const SERVERBOUND_KEY: i32 = 0x01;
    pub const SERVERBOUND_ACKNOWLEDGED: i32 = 0x03;
    pub const CLIENTBOUND_DISCONNECT: i32 = 0x00;
    pub const CLIENTBOUND_HELLO: i32 = 0x01;
    pub const CLIENTBOUND_SUCCESS: i32 = 0x02;
    pub const CLIENTBOUND_COMPRESSION: i32 = 0x03;
}

/// 配置包 ID。
pub mod config_ids {
    pub const SERVERBOUND_CLIENT_INFORMATION: i32 = 0x00;
    pub const SERVERBOUND_CUSTOM_PAYLOAD: i32 = 0x02;
    pub const SERVERBOUND_FINISH_CONFIGURATION: i32 = 0x03;
    pub const SERVERBOUND_KEEP_ALIVE: i32 = 0x04;
    pub const SERVERBOUND_PONG: i32 = 0x05;
    pub const SERVERBOUND_KNOWN_PACKS: i32 = 0x07;
    pub const CLIENTBOUND_CUSTOM_PAYLOAD: i32 = 0x01;
    pub const CLIENTBOUND_DISCONNECT: i32 = 0x02;
    pub const CLIENTBOUND_FINISH_CONFIGURATION: i32 = 0x03;
    pub const CLIENTBOUND_KEEP_ALIVE: i32 = 0x04;
    pub const CLIENTBOUND_PING: i32 = 0x05;
    pub const CLIENTBOUND_REGISTRY_DATA: i32 = 0x07;
    pub const CLIENTBOUND_UPDATE_ENABLED_FEATURES: i32 = 0x0C;
    pub const CLIENTBOUND_UPDATE_TAGS: i32 = 0x0D;
    pub const CLIENTBOUND_KNOWN_PACKS: i32 = 0x0E;
}

/// Play 包 ID（serverbound / clientbound 各自编号）。
pub mod play_ids {
    // serverbound
    pub const SERVERBOUND_CONFIRM_TELEPORT: i32 = 0x00;
    pub const SERVERBOUND_CHAT_ACK: i32 = 0x03;
    pub const SERVERBOUND_CHAT_COMMAND: i32 = 0x04;
    pub const SERVERBOUND_CHAT_COMMAND_SIGNED: i32 = 0x05;
    pub const SERVERBOUND_CHAT: i32 = 0x06;
    pub const SERVERBOUND_CHUNK_BATCH_RECEIVED: i32 = 0x08;
    pub const SERVERBOUND_CLIENT_COMMAND: i32 = 0x09;
    pub const SERVERBOUND_CLIENT_INFORMATION: i32 = 0x0A;
    pub const SERVERBOUND_COMMAND_SUGGESTION: i32 = 0x0B;
    pub const SERVERBOUND_CONTAINER_CLICK: i32 = 0x0E;
    pub const SERVERBOUND_INTERACT: i32 = 0x16;
    pub const SERVERBOUND_KEEP_ALIVE: i32 = 0x18;
    pub const SERVERBOUND_MOVE_POS: i32 = 0x1A;
    pub const SERVERBOUND_MOVE_POS_ROT: i32 = 0x1B;
    pub const SERVERBOUND_MOVE_ROT: i32 = 0x1C;
    pub const SERVERBOUND_MOVE_STATUS_ONLY: i32 = 0x1D;
    pub const SERVERBOUND_MOVE_VEHICLE: i32 = 0x1E;
    pub const SERVERBOUND_PICK_ITEM: i32 = 0x20;
    pub const SERVERBOUND_PLAYER_ABILITIES: i32 = 0x23;
    pub const SERVERBOUND_PLAYER_ACTION: i32 = 0x24;
    pub const SERVERBOUND_PLAYER_COMMAND: i32 = 0x25;
    pub const SERVERBOUND_PLAYER_INPUT: i32 = 0x26;
    pub const SERVERBOUND_SET_CARRIED_ITEM: i32 = 0x2F;
    pub const SERVERBOUND_SET_CREATIVE_MODE_SLOT: i32 = 0x32;
    pub const SERVERBOUND_SWING: i32 = 0x36;
    pub const SERVERBOUND_USE_ITEM: i32 = 0x39;
    pub const SERVERBOUND_USE_ITEM_ON: i32 = 0x38;
    pub const SERVERBOUND_CUSTOM_PAYLOAD: i32 = 0x12;
    pub const SERVERBOUND_PONG: i32 = 0x27;
    // clientbound
    pub const CLIENTBOUND_ADD_ENTITY: i32 = 0x01;
    pub const CLIENTBOUND_ADD_EXPERIENCE_ORB: i32 = 0x02;
    pub const CLIENTBOUND_ANIMATE: i32 = 0x03;
    pub const CLIENTBOUND_BLOCK_UPDATE: i32 = 0x09;
    pub const CLIENTBOUND_BUNDLE_DELIMITER: i32 = 0x00;
    pub const CLIENTBOUND_CHANGE_DIFFICULTY: i32 = 0x0B;
    pub const CLIENTBOUND_CHUNK_BATCH_FINISHED: i32 = 0x0C;
    pub const CLIENTBOUND_CHUNK_BATCH_START: i32 = 0x0D;
    pub const CLIENTBOUND_COMMAND_SUGGESTIONS: i32 = 0x10;
    pub const CLIENTBOUND_COMMANDS: i32 = 0x11;
    pub const CLIENTBOUND_CONTAINER_SET_CONTENT: i32 = 0x13;
    pub const CLIENTBOUND_CONTAINER_SET_SLOT: i32 = 0x15;
    pub const CLIENTBOUND_CUSTOM_PAYLOAD: i32 = 0x19;
    pub const CLIENTBOUND_DISCONNECT: i32 = 0x1D;
    pub const CLIENTBOUND_ENTITY_EVENT: i32 = 0x1F;
    pub const CLIENTBOUND_EXPLOSION: i32 = 0x20;
    pub const CLIENTBOUND_FORGET_LEVEL_CHUNK: i32 = 0x21;
    pub const CLIENTBOUND_GAME_EVENT: i32 = 0x22;
    pub const CLIENTBOUND_KEEP_ALIVE: i32 = 0x26;
    pub const CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT: i32 = 0x27;
    pub const CLIENTBOUND_LOGIN: i32 = 0x2B;
    pub const CLIENTBOUND_MOVE_ENTITY_POS: i32 = 0x2E;
    pub const CLIENTBOUND_MOVE_ENTITY_POS_ROT: i32 = 0x2F;
    pub const CLIENTBOUND_PING: i32 = 0x35;
    pub const CLIENTBOUND_PLAYER_ABILITIES: i32 = 0x38;
    pub const CLIENTBOUND_PLAYER_INFO_REMOVE: i32 = 0x3D;
    pub const CLIENTBOUND_PLAYER_INFO_UPDATE: i32 = 0x3E;
    pub const CLIENTBOUND_PLAYER_POSITION: i32 = 0x40;
    pub const CLIENTBOUND_REMOVE_ENTITIES: i32 = 0x42;
    pub const CLIENTBOUND_RESPAWN: i32 = 0x47;
    pub const CLIENTBOUND_SECTION_BLOCKS_UPDATE: i32 = 0x49;
    pub const CLIENTBOUND_SET_CHUNK_CACHE_CENTER: i32 = 0x54;
    pub const CLIENTBOUND_SET_CHUNK_CACHE_RADIUS: i32 = 0x55;
    pub const CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION: i32 = 0x56;
    pub const CLIENTBOUND_SET_ENTITY_DATA: i32 = 0x58;
    pub const CLIENTBOUND_SET_ENTITY_MOTION: i32 = 0x5A;
    pub const CLIENTBOUND_SET_EXPERIENCE: i32 = 0x5C;
    pub const CLIENTBOUND_SET_HEALTH: i32 = 0x5D;
    pub const CLIENTBOUND_SET_TIME: i32 = 0x64;
    pub const CLIENTBOUND_SOUND: i32 = 0x68;
    pub const CLIENTBOUND_SOUND_ENTITY: i32 = 0x67;
    pub const CLIENTBOUND_SYSTEM_CHAT: i32 = 0x6C;
    pub const CLIENTBOUND_UPDATE_TAGS: i32 = 0x78;
}

/// 帧错误。
#[derive(Debug)]
pub enum FrameError {
    /// 载荷编解码失败。
    Codec(CodecError),
    /// 压缩数据损坏或解压失败。
    Decompress(std::io::Error),
    /// 数据长度非法。
    InvalidLength(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(err) => write!(f, "codec: {err}"),
            Self::Decompress(err) => write!(f, "decompress: {err}"),
            Self::InvalidLength(len) => write!(f, "invalid frame length: {len}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(err) => Some(err),
            Self::Decompress(err) => Some(err),
            Self::InvalidLength(_) => None,
        }
    }
}

/// 协议帧最大长度（1.21.1 为 2^21 = 2 MiB）。
pub const MAX_FRAME_LENGTH: usize = 2 * 1024 * 1024;

/// 将（包 ID, 载荷）编码为完整帧。
///
/// `compression_threshold`：`None` 表示禁用压缩；`Some(t)` 表示载荷大于等于
/// `t` 字节时使用 zlib 压缩。
pub fn encode_frame(
    packet_id: i32,
    payload: &[u8],
    compression_threshold: Option<i32>,
) -> Result<Vec<u8>, FrameError> {
    let mut data = Writer::new();
    data.write_var_int(packet_id);
    data.write_slice(payload);
    let data_bytes = data.into_bytes();

    let mut frame = Writer::with_capacity(data_bytes.len() + 8);
    match compression_threshold {
        None => {
            frame.write_var_int(
                i32::try_from(data_bytes.len())
                    .map_err(|_| FrameError::InvalidLength(data_bytes.len()))?,
            );
            frame.write_slice(&data_bytes);
        }
        Some(threshold) if threshold < 0 || data_bytes.len() < threshold as usize => {
            // 未压缩：data_length = 包数据长度 + varint(0) 的 1 字节
            frame.write_var_int(
                i32::try_from(data_bytes.len() + 1)
                    .map_err(|_| FrameError::InvalidLength(data_bytes.len()))?,
            );
            frame.write_var_int(0);
            frame.write_slice(&data_bytes);
        }
        Some(_) => {
            use flate2::{Compression, write::ZlibEncoder};
            use std::io::Write;

            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder
                .write_all(&data_bytes)
                .map_err(FrameError::Decompress)?;
            let compressed = encoder.finish().map_err(FrameError::Decompress)?;
            let uncompressed_len = i32::try_from(data_bytes.len())
                .map_err(|_| FrameError::InvalidLength(data_bytes.len()))?;
            let mut uncompressed_len_buf = Writer::new();
            uncompressed_len_buf.write_var_int(uncompressed_len);
            let data_length = uncompressed_len_buf.len() + compressed.len();
            frame.write_var_int(
                i32::try_from(data_length).map_err(|_| FrameError::InvalidLength(data_length))?,
            );
            frame.write_slice(&uncompressed_len_buf.into_bytes());
            frame.write_slice(&compressed);
        }
    }
    Ok(frame.into_bytes())
}

/// 从已累积的字节流中解码一个完整帧。
///
/// 返回 `(packet_id, payload, consumed_bytes)`；数据不足时返回 `Ok(None)`。
pub fn decode_frame(
    buffer: &[u8],
    compression_threshold: Option<i32>,
) -> Result<Option<(i32, Vec<u8>, usize)>, FrameError> {
    let mut reader = Reader::new(buffer);
    // 空缓冲或不足一个 VarInt 时视为“等待更多数据”，而非错误。
    let data_len = match reader.read_var_int() {
        Ok(v) => v,
        Err(_) if buffer.is_empty() => return Ok(None),
        Err(e) => return Err(FrameError::Codec(e)),
    };
    if data_len < 0 {
        return Err(FrameError::InvalidLength(data_len as usize));
    }
    let data_len = data_len as usize;
    if data_len > MAX_FRAME_LENGTH {
        return Err(FrameError::InvalidLength(data_len));
    }
    if reader.remaining() < data_len {
        return Ok(None);
    }

    let payload_raw = &buffer[reader.position()..reader.position() + data_len];
    let consumed = reader.position() + data_len;

    let decompressed: Vec<u8> = if compression_threshold.is_none() {
        payload_raw.to_vec()
    } else {
        let mut payload_reader = Reader::new(payload_raw);
        let uncompressed_len = payload_reader.read_var_int().map_err(FrameError::Codec)?;
        let body = payload_reader.read_rest();
        if uncompressed_len == 0 {
            body.to_vec()
        } else {
            use flate2::read::ZlibDecoder;
            use std::io::Read;

            let mut decoder = ZlibDecoder::new(body);
            let mut out = Vec::with_capacity(uncompressed_len as usize);
            decoder
                .read_to_end(&mut out)
                .map_err(FrameError::Decompress)?;
            out
        }
    };

    let mut packet_reader = Reader::new(&decompressed);
    let packet_id = packet_reader.read_var_int().map_err(FrameError::Codec)?;
    let payload = packet_reader.read_rest().to_vec();
    Ok(Some((packet_id, payload, consumed)))
}

#[cfg(test)]
mod tests {
    use super::{decode_frame, encode_frame};

    #[test]
    fn uncompressed_frame_round_trip() {
        let frame = encode_frame(0x2B, &[1, 2, 3], None).unwrap();
        let (id, payload, consumed) = decode_frame(&frame, None).unwrap().unwrap();
        assert_eq!(id, 0x2B);
        assert_eq!(payload, vec![1, 2, 3]);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn compressed_frame_round_trip() {
        let payload: Vec<u8> = (0..1024u32).flat_map(u32::to_le_bytes).collect();
        let frame = encode_frame(0x2B, &payload, Some(256)).unwrap();
        let (id, decoded, consumed) = decode_frame(&frame, Some(256)).unwrap().unwrap();
        assert_eq!(id, 0x2B);
        assert_eq!(decoded, payload);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn small_payload_stays_uncompressed() {
        let frame = encode_frame(1, &[0u8; 10], Some(256)).unwrap();
        let (_, decoded, _) = decode_frame(&frame, Some(256)).unwrap().unwrap();
        assert_eq!(decoded, vec![0u8; 10]);
    }

    #[test]
    fn incomplete_frame_returns_none() {
        let frame = encode_frame(1, &[0u8; 100], None).unwrap();
        let truncated = &frame[..frame.len() - 5];
        assert!(decode_frame(truncated, None).unwrap().is_none());
    }
}
