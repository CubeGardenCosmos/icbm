//! Play 阶段 serverbound 包。

use crate::codec::{CodecError, Reader};

use super::BlockPos;
use crate::packets::play_ids::{
    SERVERBOUND_CHAT, SERVERBOUND_CHAT_COMMAND, SERVERBOUND_CHUNK_BATCH_RECEIVED,
    SERVERBOUND_CLIENT_COMMAND, SERVERBOUND_CLIENT_INFORMATION, SERVERBOUND_CONFIRM_TELEPORT,
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

/// Serverbound：聊天命令（0x04，`/` 开头消息）。
#[derive(Debug, Clone)]
pub struct SChatCommand {
    /// 命令文本（不含 `/`）。
    pub command: String,
    /// 时间戳。
    pub timestamp: i64,
    /// 盐。
    pub salt: i64,
    /// 签名（未启用时为空）。
    pub signature: Option<Vec<u8>>,
    /// 消息数量。
    pub message_count: i32,
    /// 已确认消息位集。
    pub acknowledged: Vec<i64>,
}

impl SChatCommand {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let command = reader.read_string()?;
        let timestamp = reader.read_i64_be()?;
        let salt = reader.read_i64_be()?;
        let signature = if reader.read_bool()? {
            Some(reader.read_byte_array()?.to_vec())
        } else {
            None
        };
        let message_count = reader.read_var_int()?;
        let acknowledged = reader.read_bitset()?;
        Ok(Self {
            command,
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
        SERVERBOUND_CHAT_COMMAND
    }
}

/// Serverbound：聊天消息（0x06）。
#[derive(Debug, Clone)]
pub struct SChatMessage {
    /// 消息内容。
    pub message: String,
    /// 时间戳。
    pub timestamp: i64,
    /// 盐。
    pub salt: i64,
    /// 签名（未启用时为空）。
    pub signature: Option<Vec<u8>>,
    /// 消息数量。
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
            Some(reader.read_byte_array()?.to_vec())
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
