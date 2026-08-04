//! Play 阶段 serverbound 包。

use crate::codec::{CodecError, Reader};

use super::BlockPos;
use crate::packets::play_ids::{
    SERVERBOUND_CHAT, SERVERBOUND_CHAT_ACK, SERVERBOUND_CHAT_COMMAND,
    SERVERBOUND_CHAT_COMMAND_SIGNED, SERVERBOUND_CHUNK_BATCH_RECEIVED, SERVERBOUND_CLIENT_COMMAND,
    SERVERBOUND_CLIENT_INFORMATION, SERVERBOUND_COMMAND_SUGGESTION, SERVERBOUND_CONFIRM_TELEPORT,
    SERVERBOUND_CONTAINER_CLICK, SERVERBOUND_KEEP_ALIVE, SERVERBOUND_MOVE_POS,
    SERVERBOUND_MOVE_POS_ROT, SERVERBOUND_MOVE_ROT, SERVERBOUND_MOVE_STATUS_ONLY,
    SERVERBOUND_MOVE_VEHICLE, SERVERBOUND_PICK_ITEM, SERVERBOUND_PLAYER_ABILITIES,
    SERVERBOUND_PLAYER_ACTION, SERVERBOUND_PLAYER_COMMAND, SERVERBOUND_PLAYER_INPUT,
    SERVERBOUND_PONG, SERVERBOUND_SET_CARRIED_ITEM, SERVERBOUND_SET_CREATIVE_MODE_SLOT,
    SERVERBOUND_SWING, SERVERBOUND_USE_ITEM, SERVERBOUND_USE_ITEM_ON,
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

/// Serverbound：移动载具（0x1E `VehicleMoveC2SPacket`；乘船/矿车等时替代
/// 玩家移动包）。
#[derive(Debug, Clone, Copy)]
pub struct SMoveVehicle {
    /// X。
    pub x: f64,
    /// 脚部 Y。
    pub feet_y: f64,
    /// Z。
    pub z: f64,
    /// 偏航角（度）。
    pub yaw: f32,
    /// 俯仰角（度）。
    pub pitch: f32,
}

impl SMoveVehicle {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            x: reader.read_f64_be()?,
            feet_y: reader.read_f64_be()?,
            z: reader.read_f64_be()?,
            yaw: reader.read_f32_be()?,
            pitch: reader.read_f32_be()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_MOVE_VEHICLE
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

/// Serverbound：创造模式拾取方块到物品栏（0x20 `PickFromInventoryC2SPacket`）。
///
/// 中键拾取方块时发送；字段：`VarInt slot`（对照 `PickFromInventoryC2SPacket.java`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SPickItem {
    /// 槽位。
    pub slot: i32,
}

impl SPickItem {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            slot: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PICK_ITEM
    }
}

/// Serverbound：玩家能力（0x23 `UpdatePlayerAbilitiesC2SPacket`）。
///
/// 创造/旁观模式双击空格切换飞行时发送；字段：`Bool flying`
/// （对照 `UpdatePlayerAbilitiesC2SPacket.java`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SPlayerAbilities {
    /// 是否飞行中。
    pub flying: bool,
}

impl SPlayerAbilities {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            flying: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PLAYER_ABILITIES
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

/// Serverbound：右键方块（0x38 `PlayerInteractBlockC2SPacket`；放置方块/使用方块）。
///
/// 字段（对照 `PlayerInteractBlockC2SPacket.java`）：`VarInt hand`（0=主手 1=副手）→
/// `BlockHitResult`（`BlockPos` → `VarInt face`（Direction 序：0=下 1=上 2=北 3=南 4=西 5=东）→
/// `f32 ×3` 命中偏移 → `bool` 是否在方块内）→ `VarInt sequence`。
#[derive(Debug, Clone, Copy)]
pub struct SUseItemOn {
    /// 手（0=主手，1=副手）。
    pub hand: i32,

    /// 被点击方块位置。
    pub location: BlockPos,

    /// 方块面（Direction 枚举序）。
    pub face: i32,

    /// 命中偏移 X（相对方块原点 0..1）。
    pub hit_x: f32,

    /// 命中偏移 Y。
    pub hit_y: f32,

    /// 命中偏移 Z。
    pub hit_z: f32,

    /// 是否在方块内部。
    pub inside: bool,

    /// 序列号。
    pub sequence: i32,
}

impl SUseItemOn {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            hand: reader.read_var_int()?,

            location: BlockPos::from_long(reader.read_i64_be()?),

            face: reader.read_var_int()?,

            hit_x: reader.read_f32_be()?,

            hit_y: reader.read_f32_be()?,

            hit_z: reader.read_f32_be()?,

            inside: reader.read_bool()?,

            sequence: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_USE_ITEM_ON
    }
}

/// Serverbound：右键使用物品（0x39 `PlayerInteractItemC2SPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct SUseItem {
    /// 手（0=主手，1=副手）。
    pub hand: i32,

    /// 序列号。
    pub sequence: i32,
}

impl SUseItem {
    /// 解码（对照 `PlayerInteractItemC2SPacket.java`）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            hand: reader.read_var_int()?,

            sequence: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_USE_ITEM
    }
}

/// Serverbound：切换选中槽（0x2F `UpdateSelectedSlotC2SPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct SSetCarriedItem {
    /// 选中槽（0..8）。
    pub slot: i32,
}

impl SSetCarriedItem {
    /// 解码（对照 `ServerboundSetCarriedItemPacket.java`：槽位是
    /// `readShort()`——2 字节有符号大端，不是 `VarInt`。旧实现按 `VarInt` 读会把
    /// short 的高字节（0x00）当成槽位 → 选中槽永远解码为 0，手持物品永远
    /// 是热栏第 1 格）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            slot: i32::from(reader.read_i16_be()?),
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_SET_CARRIED_ITEM
    }
}

/// Serverbound：客户端命令（0x25 `ClientCommandC2SPacket`；如起身）。
///
/// 字段：`VarInt action`（0=按下开始潜行 1=释放 2=离开床 3=开始精灵飞行）。
#[derive(Debug, Clone, Copy)]
pub struct SPlayerCommand {
    /// 动作。
    pub action: i32,
}

impl SPlayerCommand {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            action: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PLAYER_COMMAND
    }
}

/// Serverbound：玩家输入（0x26 `PlayerInputC2SPacket`）。
///
/// 字段：`f32 xxa` `f32 zza` → 7×`bool`（前进/后退/左/右/跳跃/潜行/疾跑）。
#[derive(Debug, Clone, Copy)]
pub struct SPlayerInput {
    /// 左右移动输入（-1..1）。
    pub xxa: f32,
    /// 前后移动输入（-1..1）。
    pub zza: f32,
    /// 前进。
    pub forward: bool,
    /// 后退。
    pub back: bool,
    /// 左。
    pub left: bool,
    /// 右。
    pub right: bool,
    /// 跳跃。
    pub jump: bool,
    /// 潜行。
    pub sneak: bool,
    /// 疾跑。
    pub sprint: bool,
}

impl SPlayerInput {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            xxa: reader.read_f32_be()?,
            zza: reader.read_f32_be()?,
            forward: reader.read_bool()?,
            back: reader.read_bool()?,
            left: reader.read_bool()?,
            right: reader.read_bool()?,
            jump: reader.read_bool()?,
            sneak: reader.read_bool()?,
            sprint: reader.read_bool()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PLAYER_INPUT
    }
}

/// Serverbound：创造模式物品栏操作（0x32 `CreativeInventoryActionC2SPacket`）。
///
/// 字段（对照 `CreativeInventoryActionC2SPacket.java`）：`short slot`（2 字节
/// 有符号大端，-1 = 光标/删除）→ `ItemStack`（网络编码）。
#[derive(Debug, Clone)]
pub struct SSetCreativeModeSlot {
    /// 槽位（-1 = 光标）。
    pub slot: i32,

    /// 物品（空栈表示清除）。
    pub stack: super::clientbound::CItemStack,
}

impl SSetCreativeModeSlot {
    /// 解码（对照 `CreativeInventoryActionC2SPacket.java`：槽位是
    /// `PacketCodecs.SHORT`，不是 `VarInt`——旧实现按 `VarInt` 读会把槽位后一字节
    /// 吞进 count，导致整包错位、带组件的栈读到 EOF 被踢）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let slot = i32::from(reader.read_i16_be()?);

        let stack = decode_item_stack(reader)?;

        Ok(Self { slot, stack })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_SET_CREATIVE_MODE_SLOT
    }
}

/// Serverbound：容器点击（0x0E `ServerboundContainerClickPacket`）。
///
/// 背包/容器交互（移动物品）：`click_type`（`PICKUP`/`QUICK_MOVE`/`SWAP`/`THROW`/
/// `QUICK_CRAFT`/`CLONE`/`PICKUP_ALL`）+ 槽位 + 按钮 + 变更槽快照 + 光标物品。
#[derive(Debug, Clone)]
pub struct SContainerClick {
    /// 容器窗口 ID（0 = 玩家物品栏）。
    pub container_id: i8,
    /// 状态修订号（与 `CSetContainerContent`/`CSetSlot` 的 revision 对应）。
    pub state_id: i32,
    /// 槽位（-999 = 窗口外）。
    pub slot: i16,
    /// 按钮（左 0 / 右 1 / shift、数字键 0..8、Q 丢出等）。
    pub button: i8,
    /// 点击类型（vanilla `ClickType` 序：0 `PICKUP` / 1 `QUICK_MOVE` / 2 `SWAP` /
    /// 3 `CLONE` / 4 `THROW` / 5 `QUICK_CRAFT` / 6 `PICKUP_ALL`）。
    pub click_type: i8,
    /// 变更槽快照（槽号 → 物品；客户端认为这些槽变化了）。
    pub changed_slots: Vec<(u8, super::clientbound::CItemStack)>,
    /// 客户端当前光标物品。
    pub carried: super::clientbound::CItemStack,
}

impl SContainerClick {
    /// 解码（对照 `ServerboundContainerClickPacket.java`：containerId(byte) +
    /// stateId(VarInt) + slotNum(short) + buttonNum(byte) + clickType(VarInt) +
    /// `changedSlots`（`SLOTS_STREAM_CODEC`：`VarInt` 长度 + 每项 `(byte slot, item_stack)`) +
    /// carriedItem(ItemStack)）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let container_id = reader.read_i8()?;
        let state_id = reader.read_var_int()?;
        let slot = reader.read_i16_be()?;
        let button = reader.read_i8()?;
        let click_type = reader.read_var_int()?;
        let mut changed_slots = Vec::new();
        let count = reader.read_var_int()?;
        for _ in 0..count.max(0) {
            let slot = reader.read_u8()?;
            let stack = decode_item_stack(reader)?;
            changed_slots.push((slot, stack));
        }
        let carried = decode_item_stack(reader)?;
        Ok(Self {
            container_id,
            state_id,
            slot,
            button,
            click_type: i8::try_from(click_type).unwrap_or(-1),
            changed_slots,
            carried,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CONTAINER_CLICK
    }
}

/// 解码 `ItemStack.OPTIONAL_PACKET_CODEC`（网络物品栈；含附魔组件）。
///
/// 1.21.1 `ComponentChanges.PACKET_CODEC` **没有长度前缀**：`[VarInt to_add]`
/// `[VarInt to_remove]` 后直接跟 `[VarInt component_type][组件载荷]` 列表
/// （对照 `ComponentChanges.java`）。旧实现多读一个 `len` `VarInt` 会把组件载荷
/// 首字节吞掉，带附魔的栈最终越界读取 → `unexpected eof` → 玩家被踢。
///
/// 仅 `minecraft:enchantments`（网络 ID 9）被解析；未知组件的载荷长度无法
/// 从格式得知，停止解析并返回已解析部分即可——帧按长度前缀整体消费，
/// 未消费字节被丢弃，不会造成流错位，也不会误踢合法客户端。
fn decode_item_stack(
    reader: &mut Reader<'_>,
) -> Result<super::clientbound::CItemStack, CodecError> {
    let count = reader.read_var_int()?;

    if count <= 0 {
        return Ok(super::clientbound::CItemStack::empty());
    }

    let item_id = reader.read_var_int()? - 1; // registryEntry：raw_id + 1

    let item = crate::item_ids::item_name(item_id).map(str::to_owned);

    // ComponentChanges：逐组件解析（无长度前缀）

    let to_add = reader.read_var_int()?;

    let to_remove = reader.read_var_int()?;

    let mut enchantments = Vec::new();

    let mut show_in_tooltip = true;

    for _ in 0..to_add {
        let Ok(component) = reader.read_var_int() else {
            return Ok(super::clientbound::CItemStack {
                item,
                count,
                enchantments,
                show_in_tooltip,
            });
        };

        if component == 9 {
            // minecraft:enchantments：`ItemEnchantmentsComponent.PACKET_CODEC`

            let Ok(entries) = reader.read_var_int() else {
                break;
            };

            let mut ok = true;

            for _ in 0..entries {
                let (Ok(ench_id), Ok(level)) = (reader.read_var_int(), reader.read_var_int())
                else {
                    ok = false;
                    break;
                };

                if let Some(name) = crate::enchantment_ids::enchantment_name(ench_id - 1) {
                    enchantments.push((name.to_owned(), level));
                }
            }

            if !ok {
                break;
            }

            if let Ok(show) = reader.read_bool() {
                show_in_tooltip = show;
            } else {
                break;
            }
        } else {
            // 未知组件：载荷长度不可知，停止解析（剩余字节随帧丢弃）

            break;
        }
    }

    for _ in 0..to_remove {
        let Ok(_component) = reader.read_var_int() else {
            break;
        };
    }

    Ok(super::clientbound::CItemStack {
        item,

        count,

        enchantments,

        show_in_tooltip,
    })
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

    /// vanilla 1.21.1 客户端编码的 `SET_CREATIVE_MODE_SLOT`：槽位是
    /// `short`（2 字节有符号大端），栈按 `ItemStack.OPTIONAL_PACKET_CODEC`
    /// （`ComponentChanges` 无长度前缀）。回归：旧实现按 `VarInt` 读槽位、多读
    /// `len` 前缀，带附魔的栈解码越界（`unexpected eof`）踢人。
    #[test]
    fn creative_mode_slot_decode_vanilla_wire_format() {
        // 槽位 -1（删除）+ 附魔钻石剑（sharpness III）
        let mut w = Writer::new();
        w.write_i16_be(-1);
        w.write_var_int(1); // count
        let diamond_sword = crate::item_ids::item_id("diamond_sword").unwrap() + 1;
        w.write_var_int(diamond_sword); // registryEntry：raw_id + 1
        w.write_var_int(1); // to_add
        w.write_var_int(0); // to_remove
        w.write_var_int(9); // enchantments 组件
        w.write_var_int(1); // 附魔条数
        w.write_var_int(32 + 1); // sharpness (raw 32) + 1
        w.write_var_int(3); // level
        w.write_bool(true); // showInTooltip
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SSetCreativeModeSlot::decode);
        assert_eq!(packet.slot, -1);
        assert_eq!(packet.stack.item.as_deref(), Some("diamond_sword"));
        assert_eq!(packet.stack.count, 1);
        assert_eq!(packet.stack.enchantments, vec![("sharpness".to_owned(), 3)]);

        // 槽位 5 + 普通石头（无组件）
        let mut w = Writer::new();
        w.write_i16_be(5);
        w.write_var_int(1);
        let stone = crate::item_ids::item_id("stone").unwrap() + 1;
        w.write_var_int(stone);
        w.write_var_int(0); // to_add
        w.write_var_int(0); // to_remove
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SSetCreativeModeSlot::decode);
        assert_eq!(packet.slot, 5);
        assert_eq!(packet.stack.item.as_deref(), Some("stone"));
        assert!(packet.stack.enchantments.is_empty());

        // 空栈（count = 0）
        let mut w = Writer::new();
        w.write_i16_be(0);
        w.write_var_int(0);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SSetCreativeModeSlot::decode);
        assert_eq!(packet.slot, 0);
        assert!(packet.stack.is_empty());
    }

    /// 未知组件（无长度前缀、载荷结构未知）不得把解码推向 EOF：优雅停止，
    /// 已解析部分（物品名/数量）保留，剩余字节随帧丢弃。
    #[test]
    fn creative_mode_slot_decode_unknown_component_does_not_eof() {
        let mut w = Writer::new();
        w.write_i16_be(3);
        w.write_var_int(1); // count
        let diamond_sword = crate::item_ids::item_id("diamond_sword").unwrap() + 1;
        w.write_var_int(diamond_sword);
        w.write_var_int(1); // to_add
        w.write_var_int(0); // to_remove
        w.write_var_int(5); // custom_name（未知组件：载荷为 NBT，此处给任意字节）
        w.write_slice(&[0x01, 0x02, 0x03, 0x04]); // 组件载荷（不解读）
        w.write_var_int(1); // 后续另一个组件（到不了）
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let packet =
            SSetCreativeModeSlot::decode(&mut r).expect("must not fail on unknown component");
        assert_eq!(packet.slot, 3);
        assert_eq!(packet.stack.item.as_deref(), Some("diamond_sword"));
        assert!(packet.stack.enchantments.is_empty());
    }

    #[test]
    fn pick_item_and_player_abilities_decode() {
        let mut w = Writer::new();
        w.write_var_int(7);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SPickItem::decode);
        assert_eq!(packet.slot, 7);

        let mut w = Writer::new();
        w.write_bool(true);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SPlayerAbilities::decode);
        assert!(packet.flying);
    }

    /// vanilla 1.21.1 `ServerboundSetCarriedItemPacket` 的槽位是 `readShort()`
    /// （2 字节有符号大端）：选中第 5 格（index 4）编码为 short 4 = `[0x00 0x04]`。
    /// 回归：旧实现按 VarInt 读高字节得到 0 → 选中槽永远为 0。
    #[test]
    fn set_carried_item_decode_vanilla_wire_format() {
        let mut w = Writer::new();
        w.write_i16_be(4);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SSetCarriedItem::decode);
        assert_eq!(packet.slot, 4);

        let mut w = Writer::new();
        w.write_i16_be(0);
        let bytes = w.into_bytes();
        let packet = decode_round_trip(&bytes, SSetCarriedItem::decode);
        assert_eq!(packet.slot, 0);
    }
}
