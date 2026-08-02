//! Play 阶段 serverbound 包。

use crate::codec::{CodecError, Reader};

use super::BlockPos;
use crate::packets::play_ids::{
    SERVERBOUND_CHAT, SERVERBOUND_CHAT_ACK, SERVERBOUND_CHAT_COMMAND,
    SERVERBOUND_CHAT_COMMAND_SIGNED, SERVERBOUND_CHUNK_BATCH_RECEIVED, SERVERBOUND_CLIENT_COMMAND,
    SERVERBOUND_CLIENT_INFORMATION, SERVERBOUND_COMMAND_SUGGESTION, SERVERBOUND_CONFIRM_TELEPORT,
    SERVERBOUND_KEEP_ALIVE, SERVERBOUND_MOVE_POS, SERVERBOUND_MOVE_POS_ROT, SERVERBOUND_MOVE_ROT,
    SERVERBOUND_MOVE_STATUS_ONLY, SERVERBOUND_PLAYER_ACTION, SERVERBOUND_PONG, SERVERBOUND_SWING,
};

/// Serverbound：确认传送（0x00）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SConfirmTeleport {
    /// 传送 ID。
    pub teleport_id: i32,
}

impl SConfirmTeleport {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            teleport_id: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CONFIRM_TELEPORT
    }
}

/// Serverbound：聊天命令（0x04，未签名 `CommandExecutionC2SPacket`）。
///
/// 本服务器 `enforcesSecureChat` 恒为 false（安全聊天永不支持），原版客户端
/// 以**未签名**包发送 `/` 命令，载荷仅一个命令字符串（对照
/// `CommandExecutionC2SPacket.java`：`readString()`）。签名路径见
/// [`SChatCommandSigned`]。
#[derive(Debug, Clone)]
pub struct SChatCommand {
    /// 命令文本（不含 `/`）。
    pub command: String,
}

impl SChatCommand {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            command: reader.read_string()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CHAT_COMMAND
    }
}

/// Serverbound：签名聊天命令（0x05 `ChatCommandSignedC2SPacket`）。
///
/// 仅解码结构后由调用方忽略签名数据（本服务器不校验签名、不实现签名聊天）；
/// 字段对照 `ChatCommandSignedC2SPacket.java`：command → Instant(Long) → salt →
/// `ArgumentSignatureDataMap`（`VarInt` 数量 + 每项 { String 参数名, 256 字节原始签名 }）
/// → `Acknowledgment`（`VarInt` + `BitSet`）。
#[derive(Debug, Clone)]
pub struct SChatCommandSigned {
    /// 命令文本（不含 `/`）。
    pub command: String,
    /// 时间戳（`Instant`，网络为 i64）。
    pub timestamp: i64,
    /// 盐。
    pub salt: i64,
    /// 参数签名（参数名 + 固定 256 字节签名，无长度前缀）。
    pub argument_signatures: Vec<(String, Vec<u8>)>,
    /// 消息数量（`Acknowledgment` 前半）。
    pub message_count: i32,
    /// 已确认消息位集。
    pub acknowledged: Vec<i64>,
}

impl SChatCommandSigned {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let command = reader.read_string()?;
        let timestamp = reader.read_i64_be()?;
        let salt = reader.read_i64_be()?;
        let count =
            usize::try_from(reader.read_var_int()?).map_err(|_| CodecError::InvalidLength {
                field: "argument signature count",
                len: 0,
            })?;
        let mut argument_signatures = Vec::with_capacity(count);
        for _ in 0..count {
            let name = reader.read_string()?;
            let signature = reader.read_bytes(256)?.to_vec();
            argument_signatures.push((name, signature));
        }
        let message_count = reader.read_var_int()?;
        let acknowledged = reader.read_bitset()?;
        Ok(Self {
            command,
            timestamp,
            salt,
            argument_signatures,
            message_count,
            acknowledged,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CHAT_COMMAND_SIGNED
    }
}

/// Serverbound：聊天消息（0x06 `ChatMessageC2SPacket`）。
#[derive(Debug, Clone)]
pub struct SChatMessage {
    /// 消息内容。
    pub message: String,
    /// 时间戳（`Instant`，网络为 i64）。
    pub timestamp: i64,
    /// 盐。
    pub salt: i64,
    /// 签名（未启用时为空；`MessageSignatureData.fromBuf` 固定读 256 字节，
    /// **无长度前缀**——对照 `MessageSignatureData.java`）。
    pub signature: Option<Vec<u8>>,
    /// 消息数量（`Acknowledgment` 前半）。
    pub message_count: i32,
    /// 已确认消息位集。
    pub acknowledged: Vec<i64>,
}

impl SChatMessage {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let message = reader.read_string()?;
        let timestamp = reader.read_i64_be()?;
        let salt = reader.read_i64_be()?;
        let signature = if reader.read_bool()? {
            Some(reader.read_bytes(256)?.to_vec())
        } else {
            None
        };
        let message_count = reader.read_var_int()?;
        let acknowledged = reader.read_bitset()?;
        Ok(Self {
            message,
            timestamp,
            salt,
            signature,
            message_count,
            acknowledged,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CHAT
    }
}

/// Serverbound：命令补全请求（0x0B `RequestCommandCompletionsC2SPacket`）。
///
/// 字段对照 `RequestCommandCompletionsC2SPacket.java`：`VarInt` 请求 ID +
/// 客户端输入的命令文本（含前导 `/`，上限 32500 字符）。
#[derive(Debug, Clone)]
pub struct SCommandSuggestionRequest {
    /// 请求 ID（响应包原样回显）。
    pub id: i32,
    /// 客户端输入的命令文本（含前导 `/`）。
    pub command: String,
}

impl SCommandSuggestionRequest {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            id: reader.read_var_int()?,
            command: reader.read_string()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_COMMAND_SUGGESTION
    }
}

/// Serverbound：消息确认（0x03 `MessageAcknowledgmentC2SPacket`）。
///
/// 原版客户端打开聊天框/发送消息时周期上报；服务器无需处理，仅解码丢弃。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SChatAck {
    /// 确认偏移。
    pub offset: i32,
}

impl SChatAck {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            offset: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CHAT_ACK
    }
}

/// Serverbound：区块批次已收到（0x08）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SChunkBatchReceived {
    /// 批次大小。
    pub batch_size: i32,
}

impl SChunkBatchReceived {
    /// 解码（对照 `AcknowledgeChunksC2SPacket.java`：`VarInt`）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            batch_size: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CHUNK_BATCH_RECEIVED
    }
}

/// Serverbound：客户端命令（0x09）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SClientCommand {
    /// 命令动作（0=重生，1=离开末地）。
    pub action: i32,
}

impl SClientCommand {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            action: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CLIENT_COMMAND
    }
}

/// Serverbound：客户端信息（0x0A，与配置阶段同构）。
#[derive(Debug, Clone)]
pub struct SClientInformation {
    /// 语言标签。
    pub locale: String,
    /// 可视距离。
    pub view_distance: i8,
    /// 聊天模式。
    pub chat_mode: i32,
    /// 聊天颜色。
    pub chat_colors: bool,
    /// 皮肤部件。
    pub skin_parts: u8,
    /// 主手。
    pub main_hand: i32,
    /// 文本过滤。
    pub enable_text_filtering: bool,
    /// 服务器列表可见性。
    pub allow_server_listing: bool,
}

impl SClientInformation {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            locale: reader.read_string()?,
            view_distance: reader.read_i8()?,
            chat_mode: reader.read_var_int()?,
            chat_colors: reader.read_bool()?,
            skin_parts: reader.read_u8()?,
            main_hand: reader.read_var_int()?,
            enable_text_filtering: reader.read_bool()?,
            allow_server_listing: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CLIENT_INFORMATION
    }
}

/// Serverbound：存活确认（0x18）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SKeepAlive {
    /// 存活 ID。
    pub id: i64,
}

impl SKeepAlive {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            id: reader.read_i64_be()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_KEEP_ALIVE
    }
}

/// Serverbound：仅移动位置（0x1A）。
#[derive(Debug, Clone, Copy)]
pub struct SMovePlayerPos {
    /// X。
    pub x: f64,
    /// 脚部 Y。
    pub feet_y: f64,
    /// Z。
    pub z: f64,
    /// 是否在地面。
    pub on_ground: bool,
}

impl SMovePlayerPos {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            x: reader.read_f64_be()?,
            feet_y: reader.read_f64_be()?,
            z: reader.read_f64_be()?,
            on_ground: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_MOVE_POS
    }
}

/// Serverbound：位置 + 旋转（0x1B）。
#[derive(Debug, Clone, Copy)]
pub struct SMovePlayerPosRot {
    /// X。
    pub x: f64,
    /// 脚部 Y。
    pub feet_y: f64,
    /// Z。
    pub z: f64,
    /// 偏航角。
    pub yaw: f32,
    /// 俯仰角。
    pub pitch: f32,
    /// 是否在地面。
    pub on_ground: bool,
}

impl SMovePlayerPosRot {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            x: reader.read_f64_be()?,
            feet_y: reader.read_f64_be()?,
            z: reader.read_f64_be()?,
            yaw: reader.read_f32_be()?,
            pitch: reader.read_f32_be()?,
            on_ground: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_MOVE_POS_ROT
    }
}

/// Serverbound：仅旋转（0x1C）。
#[derive(Debug, Clone, Copy)]
pub struct SMovePlayerRot {
    /// 偏航角。
    pub yaw: f32,
    /// 俯仰角。
    pub pitch: f32,
    /// 是否在地面。
    pub on_ground: bool,
}

impl SMovePlayerRot {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            yaw: reader.read_f32_be()?,
            pitch: reader.read_f32_be()?,
            on_ground: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_MOVE_ROT
    }
}

/// Serverbound：仅地面状态（0x1D）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SMovePlayerStatusOnly {
    /// 是否在地面。
    pub on_ground: bool,
}

impl SMovePlayerStatusOnly {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            on_ground: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_MOVE_STATUS_ONLY
    }
}

/// Serverbound：玩家动作（0x24）。
#[derive(Debug, Clone, Copy)]
pub struct SPlayerAction {
    /// 动作状态（0=开始破坏，1=取消破坏，2=完成破坏，3=掉落物品，4=使用物品，5=切换主手物品）。
    pub status: i32,
    /// 动作位置。
    pub location: BlockPos,
    /// 方块面（Direction 枚举 VarInt：0=下，1=上，2=北，3=南，4=西，5=东）。
    pub face: i32,
    /// 序列号。
    pub sequence: i32,
}

impl SPlayerAction {
    /// 解码（对照 `PlayerActionC2SPacket.java`：direction 为 `VarInt` 枚举）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            status: reader.read_var_int()?,
            location: BlockPos::from_long(reader.read_i64_be()?),
            face: reader.read_var_int()?,
            sequence: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PLAYER_ACTION
    }
}

/// Serverbound：pong（0x27）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SPong {
    /// 回显的 ping ID。
    pub id: i32,
}

impl SPong {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            id: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PONG
    }
}

/// Serverbound：挥手（0x36）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SSwing {
    /// 手（0=主手，1=副手）。
    pub hand: i32,
}

impl SSwing {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            hand: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_SWING
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Reader, Writer};

    fn decode_round_trip<T, F>(payload: &[u8], decode: F) -> T
    where
        F: FnOnce(&mut Reader<'_>) -> Result<T, CodecError>,
    {
        let mut r = Reader::new(payload);
        let value = decode(&mut r).unwrap();
        assert!(r.is_empty());
        value
    }

    #[test]
    fn confirm_teleport_decode() {
        let mut w = Writer::new();
        w.write_var_int(42);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SConfirmTeleport::decode);
        assert_eq!(packet.teleport_id, 42);
    }

    #[test]
    fn move_pos_decode() {
        let mut w = Writer::new();
        w.write_f64_be(1.5);
        w.write_f64_be(64.0);
        w.write_f64_be(-3.25);
        w.write_bool(true);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SMovePlayerPos::decode);
        assert_eq!(packet.x, 1.5);
        assert_eq!(packet.feet_y, 64.0);
        assert_eq!(packet.z, -3.25);
        assert!(packet.on_ground);
    }

    #[test]
    fn chat_message_decode() {
        let mut w = Writer::new();
        w.write_string("hello world");
        w.write_i64_be(1_700_000_000);
        w.write_i64_be(123);
        w.write_bool(false);
        w.write_var_int(3);
        w.write_bitset(&[0b101]);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SChatMessage::decode);
        assert_eq!(packet.message, "hello world");
        assert!(packet.signature.is_none());
        assert_eq!(packet.message_count, 3);
        assert_eq!(packet.acknowledged, vec![0b101]);
    }

    #[test]
    fn chat_message_signature_is_raw_256_bytes() {
        // `MessageSignatureData.fromBuf` 固定读 256 字节，无长度前缀
        let mut w = Writer::new();
        w.write_string("signed msg");
        w.write_i64_be(0);
        w.write_i64_be(0);
        w.write_bool(true);
        w.write_slice(&[0xAB; 256]);
        w.write_var_int(0);
        w.write_var_int(0);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SChatMessage::decode);
        assert_eq!(packet.signature.as_deref(), Some([0xAB; 256].as_slice()));
    }

    #[test]
    fn chat_command_decode_is_unsigned_string_only() {
        // `enforcesSecureChat=false` 时客户端发送 `CommandExecutionC2SPacket`：
        // 载荷仅一个命令字符串，多出的字段读取会 eof 失败。
        let mut w = Writer::new();
        w.write_string("gamemode creative");
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SChatCommand::decode);
        assert_eq!(packet.command, "gamemode creative");

        // 旧实现误按签名结构解码：字符串后紧跟时间戳字段 → UnexpectedEof。
        let mut short = Writer::new();
        short.write_string("gamemode");
        let short = short.into_bytes();
        let mut r = Reader::new(&short);
        assert!(SChatCommand::decode(&mut r).is_ok());
        assert!(r.is_empty());
    }

    #[test]
    fn chat_command_signed_decode() {
        let mut w = Writer::new();
        w.write_string("gamemode creative");
        w.write_i64_be(1_700_000_000);
        w.write_i64_be(42);
        w.write_var_int(1); // ArgumentSignatureDataMap 数量
        w.write_string("mode");
        w.write_slice(&[0xCD; 256]);
        w.write_var_int(7);
        w.write_bitset(&[0b11]);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SChatCommandSigned::decode);
        assert_eq!(packet.command, "gamemode creative");
        assert_eq!(packet.timestamp, 1_700_000_000);
        assert_eq!(packet.salt, 42);
        assert_eq!(packet.argument_signatures.len(), 1);
        assert_eq!(packet.argument_signatures[0].0, "mode");
        assert_eq!(packet.argument_signatures[0].1, vec![0xCD; 256]);
        assert_eq!(packet.message_count, 7);
        assert_eq!(packet.acknowledged, vec![0b11]);
    }

    #[test]
    fn command_suggestion_request_decode() {
        let mut w = Writer::new();
        w.write_var_int(7);
        w.write_string("/gamemode cr");
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SCommandSuggestionRequest::decode);
        assert_eq!(packet.id, 7);
        assert_eq!(packet.command, "/gamemode cr");
    }

    #[test]
    fn chat_ack_decode() {
        let mut w = Writer::new();
        w.write_var_int(3);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SChatAck::decode);
        assert_eq!(packet.offset, 3);
    }

    #[test]
    fn player_action_decode() {
        let mut w = Writer::new();
        w.write_var_int(2);
        w.write_i64_be(BlockPos::new(10, 64, -5).as_long());
        w.write_var_int(1);
        w.write_var_int(7);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SPlayerAction::decode);
        assert_eq!(packet.status, 2);
        assert_eq!(packet.location, BlockPos::new(10, 64, -5));
        assert_eq!(packet.face, 1);
        assert_eq!(packet.sequence, 7);
    }
}
