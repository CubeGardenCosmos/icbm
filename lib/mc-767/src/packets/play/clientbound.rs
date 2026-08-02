//! Play 阶段 clientbound 包。

use uuid::Uuid;

use crate::codec::Writer;
use crate::nbt::{Nbt, NbtCompound};
use crate::packets::config::Tags;
use crate::packets::play::{
    BlockPos, HEIGHTMAP_COLUMNS, encode_heightmap, encode_paletted_container,
};
use crate::packets::play_ids::{
    CLIENTBOUND_ADD_ENTITY, CLIENTBOUND_ADD_EXPERIENCE_ORB, CLIENTBOUND_ANIMATE,
    CLIENTBOUND_BLOCK_UPDATE, CLIENTBOUND_BUNDLE_DELIMITER, CLIENTBOUND_CHANGE_DIFFICULTY,
    CLIENTBOUND_CHUNK_BATCH_FINISHED, CLIENTBOUND_CHUNK_BATCH_START,
    CLIENTBOUND_COMMAND_SUGGESTIONS, CLIENTBOUND_COMMANDS, CLIENTBOUND_CONTAINER_SET_SLOT,
    CLIENTBOUND_DISCONNECT, CLIENTBOUND_ENTITY_EVENT, CLIENTBOUND_EXPLOSION,
    CLIENTBOUND_FORGET_LEVEL_CHUNK, CLIENTBOUND_GAME_EVENT, CLIENTBOUND_KEEP_ALIVE,
    CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT, CLIENTBOUND_LOGIN, CLIENTBOUND_MOVE_ENTITY_POS,
    CLIENTBOUND_MOVE_ENTITY_POS_ROT, CLIENTBOUND_PING, CLIENTBOUND_PLAYER_ABILITIES,
    CLIENTBOUND_PLAYER_INFO_REMOVE, CLIENTBOUND_PLAYER_INFO_UPDATE, CLIENTBOUND_PLAYER_POSITION,
    CLIENTBOUND_REMOVE_ENTITIES, CLIENTBOUND_RESPAWN, CLIENTBOUND_SECTION_BLOCKS_UPDATE,
    CLIENTBOUND_SET_CHUNK_CACHE_CENTER, CLIENTBOUND_SET_CHUNK_CACHE_RADIUS,
    CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION, CLIENTBOUND_SET_ENTITY_MOTION,
    CLIENTBOUND_SET_EXPERIENCE, CLIENTBOUND_SET_HEALTH, CLIENTBOUND_SET_TIME,
    CLIENTBOUND_SOUND, CLIENTBOUND_SOUND_ENTITY, CLIENTBOUND_SYSTEM_CHAT, CLIENTBOUND_UPDATE_TAGS,
};

/// 区块 section 的网络数据（4096 个方块状态 ID + 64 个生物群系 ID）。
#[derive(Debug, Clone)]
pub struct ChunkSection {
    /// 非空气方块数。
    pub non_air_block_count: i16,
    /// 4096 个方块状态网络 ID（y 优先序：`(y << 4 | z) << 4 | x`）。
    pub block_states: Vec<i32>,
    /// 64 个生物群系网络 ID。
    pub biomes: Vec<i32>,
}

impl ChunkSection {
    /// 全空气 section。
    #[must_use]
    pub fn air(biome_id: i32) -> Self {
        Self {
            non_air_block_count: 0,
            block_states: vec![0; 4096],
            biomes: vec![biome_id; 64],
        }
    }

    /// 从 16×16×16 方块状态 ID 数组构造（同时计算非空气数）。
    #[must_use]
    pub fn from_block_ids(blocks: &[i32; 4096], biome_id: i32) -> Self {
        let non_air = blocks.iter().filter(|&&id| id != 0).count();
        Self {
            non_air_block_count: i16::try_from(non_air).expect("section block count exceeds i16"),
            block_states: blocks.to_vec(),
            biomes: vec![biome_id; 64],
        }
    }
}

/// Clientbound：实体生成（0x01 `EntitySpawnS2CPacket.java`）。
///
/// 字段顺序对照 `EntitySpawnS2CPacket.write`：entityId(VarInt) → UUID →
/// entityType（注册表 `VarInt`）→ x/y/z(`F64`) → pitch/yaw/headYaw(`Byte`，`floor(deg*256/360)`) →
/// entityData(VarInt，1.21.1 仍编码但常为 0) → velocity(Short × 3，`clamp(-3.9,3.9)*8000`)。
#[derive(Debug, Clone)]
pub struct CAddEntity {
    /// 实体 ID。
    pub entity_id: i32,
    /// 实体 UUID。
    pub uuid: Uuid,
    /// 实体类型注册表 ID（如 `minecraft:creeper` = 23）。
    pub entity_type: i32,
    /// X。
    pub x: f64,
    /// Y。
    pub y: f64,
    /// Z。
    pub z: f64,
    /// 俯仰角（度）。
    pub pitch: f32,
    /// 偏航角（度）。
    pub yaw: f32,
}

impl CAddEntity {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.entity_id);
        writer.write_uuid(&self.uuid);
        writer.write_var_int(self.entity_type);
        writer.write_f64_be(self.x);
        writer.write_f64_be(self.y);
        writer.write_f64_be(self.z);
        writer.write_i8(angle_byte(self.pitch));
        writer.write_i8(angle_byte(self.yaw));
        writer.write_i8(angle_byte(self.yaw)); // headYaw
        writer.write_var_int(0); // entityData
        writer.write_i16_be(0); // velocityX
        writer.write_i16_be(0); // velocityY
        writer.write_i16_be(0); // velocityZ
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_ADD_ENTITY
    }
}

/// 度 → 网络角度字节（`MathHelper.floor(deg * 256 / 360)`）。
#[must_use]
pub const fn angle_byte(degrees: f32) -> i8 {
    (degrees * 256.0 / 360.0) as i8
}

/// Clientbound：实体相对移动（0x2E `EntityS2CPacket.MoveRelative`）。
///
/// delta 单位 1/4096 方块（`EntityTrackerEntry` 的 `(x - prevX) * 4096`）。
#[derive(Debug, Clone)]
pub struct CMoveEntityPos {
    /// 实体 ID。
    pub entity_id: i32,
    /// X 增量（1/4096 方块）。
    pub delta_x: i16,
    /// Y 增量（1/4096 方块）。
    pub delta_y: i16,
    /// Z 增量（1/4096 方块）。
    pub delta_z: i16,
    /// 是否在地面。
    pub on_ground: bool,
}

impl CMoveEntityPos {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.entity_id);
        writer.write_i16_be(self.delta_x);
        writer.write_i16_be(self.delta_y);
        writer.write_i16_be(self.delta_z);
        writer.write_bool(self.on_ground);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_MOVE_ENTITY_POS
    }
}

/// Clientbound：实体相对移动+旋转（0x2F `EntityS2CPacket.RotateAndMoveRelative`）。
#[derive(Debug, Clone)]
pub struct CMoveEntityPosRot {
    /// 实体 ID。
    pub entity_id: i32,
    /// X 增量（1/4096 方块）。
    pub delta_x: i16,
    /// Y 增量（1/4096 方块）。
    pub delta_y: i16,
    /// Z 增量（1/4096 方块）。
    pub delta_z: i16,
    /// 偏航角字节。
    pub yaw: i8,
    /// 俯仰角字节。
    pub pitch: i8,
    /// 是否在地面。
    pub on_ground: bool,
}

impl CMoveEntityPosRot {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.entity_id);
        writer.write_i16_be(self.delta_x);
        writer.write_i16_be(self.delta_y);
        writer.write_i16_be(self.delta_z);
        writer.write_i8(self.yaw);
        writer.write_i8(self.pitch);
        writer.write_bool(self.on_ground);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_MOVE_ENTITY_POS_ROT
    }
}

/// Clientbound：移除实体（0x42 `EntitiesDestroyS2CPacket`）。
#[derive(Debug, Clone)]
pub struct CRemoveEntities {
    /// 实体 ID 列表。
    pub entity_ids: Vec<i32>,
}

impl CRemoveEntities {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(i32::try_from(self.entity_ids.len()).expect("ids exceed i32"));
        for id in &self.entity_ids {
            writer.write_var_int(*id);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_REMOVE_ENTITIES
    }
}

/// Clientbound：区块数据（0x27，含光照）。
#[derive(Debug, Clone)]
pub struct CLevelChunkWithLight {
    /// 区块 X。
    pub x: i32,
    /// 区块 Z。
    pub z: i32,
    /// 维度最低 Y（heightmap 存储值 = 列高 − `min_y`）。
    pub min_y: i32,
    /// 列高（256 列，值 = 最高非空气方块 y + 1，绝对 Y）。
    pub world_surface_heights: [i32; HEIGHTMAP_COLUMNS],
    /// 自底向上的 section 列表。
    pub sections: Vec<ChunkSection>,
    /// 每世界 section 的天空光（2048 字节全 0xF；空表示无）。
    pub sky_light: Vec<Vec<u8>>,
    /// 每世界 section 的方块光（2048 字节；空表示无）。
    pub block_light: Vec<Vec<u8>>,
}

impl CLevelChunkWithLight {
    /// 构造 flat 世界区块（全天空光）。
    #[must_use]
    pub fn flat(
        x: i32,
        z: i32,
        min_y: i32,
        heights: [i32; HEIGHTMAP_COLUMNS],
        sections: Vec<ChunkSection>,
    ) -> Self {
        let full = vec![0xFFu8; 2048];
        let sky_light = vec![full; sections.len()];
        Self {
            x,
            z,
            min_y,
            world_surface_heights: heights,
            sections,
            block_light: Vec::new(),
            sky_light,
        }
    }

    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i32_be(self.x);
        writer.write_i32_be(self.z);

        // Heightmaps（未命名 NBT：WORLD_SURFACE + MOTION_BLOCKING）
        let motion = encode_heightmap(&self.world_surface_heights, self.min_y);
        let mut heightmap = NbtCompound::new();
        heightmap.insert("WORLD_SURFACE", Nbt::LongArray(Box::new(motion)));
        let motion_blocking = encode_heightmap(&self.world_surface_heights, self.min_y);
        heightmap.insert("MOTION_BLOCKING", Nbt::LongArray(Box::new(motion_blocking)));
        writer
            .write_nbt_unnamed(&Nbt::Compound(Box::new(heightmap)))
            .expect("heightmap nbt must encode");

        // sections：方块 + 生物群系
        let mut blocks_and_biomes = Writer::new();
        for section in &self.sections {
            blocks_and_biomes.write_i16_be(section.non_air_block_count);
            encode_paletted_container(
                &mut blocks_and_biomes,
                &section.block_states,
                8,
                |bits| bits.max(4),
                15,
            );
            encode_paletted_container(&mut blocks_and_biomes, &section.biomes, 3, |bits| bits, 6);
        }
        writer.write_var_int(i32::try_from(blocks_and_biomes.len()).expect("sections exceed i32"));
        writer.write_slice(&blocks_and_biomes.into_bytes());

        // 方块实体（Phase 1 为空）
        writer.write_var_int(0);

        // 光照掩码（对照 vanilla `LightData.java`：掩码覆盖世界最低 section 前一个
        // section 至世界上方一个 section，共 n+2 位：bit 0 = 世界下方 section，
        // bit 1..=n = 世界 section，bit n+1 = 世界上方 section；下方/上方无光照
        // 数据 → 写入空掩码）。flat 世界天空光全亮、方块光全空。
        let num_sections = self.sections.len();
        let mut sky_light_mask = 0i64;
        let mut block_light_mask = 0i64;
        let mut sky_light_empty_mask = 0i64;
        let mut block_light_empty_mask = 0i64;

        // bit 0 = 世界下方 section（始终无数据）
        sky_light_empty_mask |= 1;
        block_light_empty_mask |= 1;

        for section_index in 0..num_sections {
            let bit = 1i64 << (section_index + 1);
            if section_index < self.sky_light.len() {
                sky_light_mask |= bit;
            } else {
                sky_light_empty_mask |= bit;
            }
            if section_index < self.block_light.len() {
                block_light_mask |= bit;
            } else {
                block_light_empty_mask |= bit;
            }
        }

        // bit n+1 = 世界上方 section（始终无数据）
        sky_light_empty_mask |= 1i64 << (num_sections + 1);
        block_light_empty_mask |= 1i64 << (num_sections + 1);

        writer.write_bitset(&[sky_light_mask]);
        writer.write_bitset(&[block_light_mask]);
        writer.write_bitset(&[sky_light_empty_mask]);
        writer.write_bitset(&[block_light_empty_mask]);

        // 天空光数组
        writer.write_var_int(
            i32::try_from(self.sky_light.len()).expect("sky light sections exceed i32"),
        );
        for data in &self.sky_light {
            writer.write_var_int(i32::try_from(data.len()).expect("light data exceeds i32"));
            writer.write_slice(data);
        }

        // 方块光数组
        writer.write_var_int(
            i32::try_from(self.block_light.len()).expect("light sections exceed i32"),
        );
        for data in &self.block_light {
            writer.write_var_int(i32::try_from(data.len()).expect("light data exceeds i32"));
            writer.write_slice(data);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT
    }
}

/// 加入游戏数据（PlayerSpawnInfo）。
#[derive(Debug, Clone)]
pub struct PlayerSpawnData {
    /// 维度类型网络 ID。
    pub dimension_type: i32,
    /// 维度名（如 `minecraft:overworld`）。
    pub dimension_name: String,
    /// 世界种子哈希。
    pub hashed_seed: i64,
    /// 游戏模式（0=生存，1=创造，2=冒险，3=旁观）。
    pub game_mode: u8,
    /// 上一游戏模式（-1 表示无）。
    pub previous_gamemode: i8,
    /// 调试世界。
    pub is_debug: bool,
    /// 平坦世界。
    pub is_flat: bool,
    /// 死亡维度与位置（用于重生罗盘）。
    pub death_dimension: Option<(String, BlockPos)>,
    /// 传送门冷却。
    pub portal_cooldown: i32,
}

/// Clientbound：加入游戏（0x2B）。
#[derive(Debug, Clone)]
pub struct CLogin {
    /// 玩家实体 ID。
    pub entity_id: i32,
    /// 硬核模式。
    pub is_hardcore: bool,
    /// 维度名列表。
    pub dimension_names: Vec<String>,
    /// 最大玩家数。
    pub max_players: i32,
    /// 可视距离。
    pub view_distance: i32,
    /// 模拟距离。
    pub simulation_distance: i32,
    /// 减少调试信息。
    pub reduced_debug_info: bool,
    /// 启用重生界面。
    pub enable_respawn_screen: bool,
    /// 限制合成。
    pub limited_crafting: bool,
    /// 出生信息。
    pub spawn_data: PlayerSpawnData,
    /// 强制安全聊天（开发约束：永不支持安全聊天，恒为 false）。
    pub enforces_secure_chat: bool,
}

impl CLogin {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i32_be(self.entity_id);
        writer.write_bool(self.is_hardcore);
        writer.write_var_int(
            i32::try_from(self.dimension_names.len()).expect("dimensions exceed i32"),
        );
        for name in &self.dimension_names {
            writer.write_string(name);
        }
        writer.write_var_int(self.max_players);
        writer.write_var_int(self.view_distance);
        writer.write_var_int(self.simulation_distance);
        writer.write_bool(self.reduced_debug_info);
        writer.write_bool(self.enable_respawn_screen);
        writer.write_bool(self.limited_crafting);
        writer.write_var_int(self.spawn_data.dimension_type + 1);
        writer.write_string(&self.spawn_data.dimension_name);
        writer.write_i64_be(self.spawn_data.hashed_seed);
        writer.write_u8(self.spawn_data.game_mode);
        writer.write_i8(self.spawn_data.previous_gamemode);
        writer.write_bool(self.spawn_data.is_debug);
        writer.write_bool(self.spawn_data.is_flat);
        match &self.spawn_data.death_dimension {
            Some((name, pos)) => {
                writer.write_bool(true);
                writer.write_string(name);
                writer.write_i64_be(pos.as_long());
            }
            None => writer.write_bool(false),
        }
        writer.write_var_int(self.spawn_data.portal_cooldown);
        writer.write_bool(self.enforces_secure_chat);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_LOGIN
    }
}

/// Clientbound：束分隔符（0x00，空载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CBundleDelimiter;

impl CBundleDelimiter {
    /// 编码载荷。
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)]
    pub const fn encode(&self, _writer: &mut Writer) {}

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_BUNDLE_DELIMITER
    }
}

/// Clientbound：改变难度（0x0B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CChangeDifficulty {
    /// 难度（0=和平…3=困难）。
    pub difficulty: u8,
    /// 是否锁定。
    pub locked: bool,
}

impl CChangeDifficulty {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_u8(self.difficulty);
        writer.write_bool(self.locked);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_CHANGE_DIFFICULTY
    }
}

/// Clientbound：区块批次开始（0x0D，空载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CChunkBatchStart;

impl CChunkBatchStart {
    /// 编码载荷。
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)]
    pub const fn encode(&self, _writer: &mut Writer) {}

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_CHUNK_BATCH_START
    }
}

/// Clientbound：区块批次完成（0x0C）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CChunkBatchFinished {
    /// 批次内区块数。
    pub batch_size: i32,
}

impl CChunkBatchFinished {
    /// 编码载荷（对照 `ChunkSentS2CPacket.java`：`VarInt`）。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.batch_size);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_CHUNK_BATCH_FINISHED
    }
}

/// 命令树节点（1.21.1 `CommandTreeS2CPacket` 的节点数据）。
///
/// 仅支持本服务器用到的节点形态：根、字面量、字符串参数；不编码
/// redirect(0x08)/executable(0x04)/customSuggestions(0x10) 标志。
#[derive(Debug, Clone)]
pub enum CommandTreeNode {
    /// 根节点（flags=0）。
    Root {
        /// 子节点下标。
        children: Vec<usize>,
    },
    /// 字面量节点（flags=1，`LiteralCommandNode`）。
    Literal {
        /// 命令名（如 `gamemode`）。
        name: String,
        /// 子节点下标。
        children: Vec<usize>,
    },
    /// 参数节点（flags=2，`RequiredArgumentNode`）。
    ///
    /// 参数类型固定为 `brigadier:string`（注册序 5），word 变体；该类型无内置
    /// 建议源，客户端对参数补全会发 `RequestCommandCompletionsC2SPacket` 请求服务器。
    Argument {
        /// 参数名（客户端显示占位符 `<name>`）。
        name: String,
        /// 子节点下标。
        children: Vec<usize>,
    },
}

/// Clientbound：命令树（0x11 `CommandTreeS2CPacket`）。
///
/// 编码对照 `CommandTreeS2CPacket.write`：`writeCollection`（`VarInt` 节点数）→
/// 每节点 `flags(Byte)` → `writeIntArray(children)`（`VarInt` 数量 + `VarInt` 下标）→
/// literal/argument 数据 → 最后 `writeVarInt(rootIndex)`（根下标在**末尾**）。
#[derive(Debug, Clone)]
pub struct CCommands {
    /// 节点表（下标 0 为根节点）。
    pub nodes: Vec<CommandTreeNode>,
}

impl CCommands {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(i32::try_from(self.nodes.len()).expect("nodes exceed i32"));
        for node in &self.nodes {
            let (flags, children) = match node {
                CommandTreeNode::Root { children } => (0u8, children.as_slice()),
                CommandTreeNode::Literal { children, .. } => (1u8, children.as_slice()),
                CommandTreeNode::Argument { children, .. } => (2u8, children.as_slice()),
            };
            writer.write_u8(flags);
            writer.write_var_int(i32::try_from(children.len()).expect("children exceed i32"));
            for child in children {
                writer.write_var_int(i32::try_from(*child).expect("child index exceeds i32"));
            }
            match node {
                CommandTreeNode::Root { .. } => {}
                CommandTreeNode::Literal { name, .. } => writer.write_string(name),
                CommandTreeNode::Argument { name, .. } => {
                    writer.write_string(name);
                    // `brigadier:string`（`ArgumentTypes.java` 注册序 5），properties =
                    // StringType 枚举 VarInt：0=word（对照 `StringArgumentType.Serializer`）。
                    writer.write_var_int(5);
                    writer.write_var_int(0);
                }
            }
        }
        writer.write_var_int(0); // rootIndex（0 = 第一个节点）
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_COMMANDS
    }
}

/// Clientbound：命令补全建议（0x10 `CommandSuggestionsS2CPacket`）。
///
/// 编码对照 `CommandSuggestionsS2CPacket.java`：id → start → length → 建议列表；
/// 每项 { String 文本, Optional<Text> tooltip }，tooltip 为 NBT 组件，不发送时写 false。
#[derive(Debug, Clone)]
pub struct CCommandSuggestions {
    /// 请求 ID（回显客户端 `RequestCommandCompletionsC2SPacket` 的 ID）。
    pub id: i32,
    /// 替换区间起点（相对客户端原始输入字符串，含前导 `/`）。
    pub start: i32,
    /// 替换区间长度。
    pub length: i32,
    /// 建议文本列表。
    pub suggestions: Vec<String>,
}

impl CCommandSuggestions {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.id);
        writer.write_var_int(self.start);
        writer.write_var_int(self.length);
        writer
            .write_var_int(i32::try_from(self.suggestions.len()).expect("suggestions exceed i32"));
        for text in &self.suggestions {
            writer.write_string(text);
            writer.write_bool(false); // 无 tooltip（TextCodecs.OPTIONAL_UNLIMITED_REGISTRY_PACKET_CODEC）
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_COMMAND_SUGGESTIONS
    }
}

/// Clientbound：断开（0x1D）。
#[derive(Debug, Clone)]
pub struct CDisconnect {
    /// 断开原因 JSON 文本组件。
    pub reason: String,
}

impl CDisconnect {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_string(&self.reason);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_DISCONNECT
    }
}

/// Clientbound：遗忘区块（0x21）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CForgetLevelChunk {
    /// 区块 X。
    pub x: i32,
    /// 区块 Z。
    pub z: i32,
}

impl CForgetLevelChunk {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i32_be(self.x);
        writer.write_i32_be(self.z);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_FORGET_LEVEL_CHUNK
    }
}

/// Clientbound：游戏事件（0x22）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CGameEvent {
    /// 事件 ID。
    pub event: u8,
    /// 事件值。
    pub value: f32,
}

impl CGameEvent {
    /// 区块即将到达（进服时发送；客户端据此显示加载界面）。
    pub const EVENT_INITIAL_CHUNKS_COMING: u8 = 13;
    /// 游戏模式变更。
    pub const EVENT_GAME_MODE_CHANGED: u8 = 3;

    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_u8(self.event);
        writer.write_f32_be(self.value);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_GAME_EVENT
    }
}

/// Clientbound：存活检测（0x26）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CKeepAlive {
    /// 存活 ID。
    pub id: i64,
}

impl CKeepAlive {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i64_be(self.id);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_KEEP_ALIVE
    }
}

/// Clientbound：ping（0x35）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CPing {
    /// ping ID，客户端原样回显。
    pub id: i32,
}

impl CPing {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.id);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_PING
    }
}

/// Clientbound：玩家能力（0x38）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CPlayerAbilities {
    /// 能力位掩码。
    pub flags: u8,
    /// 飞行速度。
    pub flying_speed: f32,
    /// 视场角修正。
    pub fov_modifier: f32,
}

impl CPlayerAbilities {
    /// 无敌。
    pub const FLAG_INVULNERABLE: u8 = 0x01;
    /// 正在飞行。
    pub const FLAG_FLYING: u8 = 0x02;
    /// 允许飞行。
    pub const FLAG_ALLOW_FLYING: u8 = 0x04;
    /// 创造模式。
    pub const FLAG_CREATIVE: u8 = 0x08;

    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_u8(self.flags);
        writer.write_f32_be(self.flying_speed);
        writer.write_f32_be(self.fov_modifier);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_PLAYER_ABILITIES
    }
}

/// 玩家信息条目（Phase 1 固定发送 `ADD_PLAYER` + `GAME_MODE` + `LISTED` + `LATENCY`）。
#[derive(Debug, Clone)]
pub struct PlayerInfoEntry {
    /// 玩家 UUID。
    pub uuid: Uuid,
    /// 玩家名。
    pub name: String,
    /// 游戏模式。
    pub game_mode: i32,
    /// 是否列入 Tab 列表。
    pub listed: bool,
    /// 延迟（毫秒）。
    pub latency: i32,
}

/// Clientbound：玩家信息更新（0x3E）。
#[derive(Debug, Clone, Default)]
pub struct CPlayerInfoUpdate {
    /// 玩家条目。
    pub players: Vec<PlayerInfoEntry>,
}

impl CPlayerInfoUpdate {
    /// 编码载荷（`ADD_PLAYER` | `UPDATE_GAME_MODE` | `UPDATE_LISTED` | `UPDATE_LATENCY`）。
    pub fn encode(&self, writer: &mut Writer) {
        const FLAGS: u8 = 0x01 | 0x04 | 0x08 | 0x10;
        writer.write_u8(FLAGS);
        writer.write_var_int(i32::try_from(self.players.len()).expect("players exceed i32"));
        for player in &self.players {
            writer.write_uuid(&player.uuid);
            // ADD_PLAYER
            writer.write_string(&player.name);
            writer.write_var_int(0); // 属性数量
            // UPDATE_GAME_MODE
            writer.write_var_int(player.game_mode);
            // UPDATE_LISTED
            writer.write_bool(player.listed);
            // UPDATE_LATENCY
            writer.write_var_int(player.latency);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_PLAYER_INFO_UPDATE
    }
}

/// Clientbound：移除玩家信息（0x3D）。
#[derive(Debug, Clone, Default)]
pub struct CPlayerInfoRemove {
    /// 玩家 UUID 列表。
    pub uuids: Vec<Uuid>,
}

impl CPlayerInfoRemove {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(i32::try_from(self.uuids.len()).expect("uuids exceed i32"));
        for uuid in &self.uuids {
            writer.write_uuid(uuid);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_PLAYER_INFO_REMOVE
    }
}

/// Clientbound：玩家位置（0x40）。
#[derive(Debug, Clone, Copy)]
pub struct CPlayerPosition {
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
    /// 相对移动位掩码（0 = 绝对）。
    pub flags: u8,
    /// 传送 ID。
    pub teleport_id: i32,
}

impl CPlayerPosition {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_f64_be(self.x);
        writer.write_f64_be(self.y);
        writer.write_f64_be(self.z);
        writer.write_f32_be(self.yaw);
        writer.write_f32_be(self.pitch);
        writer.write_u8(self.flags);
        writer.write_var_int(self.teleport_id);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_PLAYER_POSITION
    }
}

/// Clientbound：设置区块缓存中心（0x54）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CSetChunkCacheCenter {
    /// 区块 X。
    pub chunk_x: i32,
    /// 区块 Z。
    pub chunk_z: i32,
}

impl CSetChunkCacheCenter {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.chunk_x);
        writer.write_var_int(self.chunk_z);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_CHUNK_CACHE_CENTER
    }
}

/// Clientbound：设置区块缓存半径（0x55）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CSetChunkCacheRadius {
    /// 半径（区块）。
    pub radius: i32,
}

impl CSetChunkCacheRadius {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.radius);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_CHUNK_CACHE_RADIUS
    }
}

/// Clientbound：设置默认出生点（0x56）。
#[derive(Debug, Clone, Copy)]
pub struct CSetDefaultSpawnPosition {
    /// 出生点方块位置。
    pub position: BlockPos,
    /// 出生朝向角。
    pub angle: f32,
}

impl CSetDefaultSpawnPosition {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i64_be(self.position.as_long());
        writer.write_f32_be(self.angle);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION
    }
}

/// Clientbound：系统聊天（0x6C）。
///
/// 内容为纯文本；网络序列化为 NBT 组件（`{text: ...}`，客户端
/// `FriendlyByteBuf.readNbt` 解析），非裸 JSON 字符串。
#[derive(Debug, Clone)]
pub struct CSystemChat {
    /// 内容纯文本。
    pub content: String,
    /// 是否覆盖模式（Action Bar）。
    pub overlay: bool,
}

impl CSystemChat {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        let mut component = NbtCompound::new();
        component.insert("text", Nbt::String(self.content.clone()));
        writer
            .write_nbt_unnamed(&Nbt::Compound(Box::new(component)))
            .expect("system chat component must encode");
        writer.write_bool(self.overlay);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SYSTEM_CHAT
    }
}

/// Clientbound：更新标签（0x78，与配置阶段同构）。
#[derive(Debug, Clone, Default)]
pub struct CUpdateTags {
    /// 标签数据。
    pub tags: Tags,
}

impl CUpdateTags {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        self.tags.encode(writer);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_UPDATE_TAGS
    }
}

/// 平坦世界 biomes 常量（供 region 复用）。
pub mod flat_biomes {
    use super::super::biome_ids;

    /// 主世界平坦生物群系。
    pub const OVERWORLD: i32 = biome_ids::PLAINS;
    /// 地狱平坦生物群系。
    pub const NETHER: i32 = biome_ids::NETHER_WASTES;
    /// 末地平坦生物群系。
    pub const END: i32 = biome_ids::THE_END;
}

/// 物品网络编码（1.21.1 `ItemStack.OPTIONAL_PACKET_CODEC`）。
///
/// 字段：`VarInt count`（0 = 空栈）→ `VarInt item_id + 1`（`registryEntry`：
/// 引用条目写 `raw_id + 1`，直接条目写 0）→ `ComponentChanges`（`VarInt 添加数`
/// + `VarInt 移除数` + 组件数据）。
///
/// 本实现支持：空栈、无组件栈、`minecraft:enchantments` 组件（附魔系统用）。
/// 其余组件类型（`max_stack_size`/`attribute_modifiers` 等）不在服务端显式写入——
/// 客户端对缺失组件应用物品默认值，与 vanilla 下发省略默认组件一致。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CItemStack {
    /// 物品注册表名（如 `minecraft:stone`；空 = 空栈）。
    pub item: Option<String>,
    /// 数量。
    pub count: i32,
    /// 附魔（enchantments 组件，顺序即服务端收到的顺序）。
    pub enchantments: Vec<(String, i32)>,
    /// 是否在工具提示中显示附魔。
    pub show_in_tooltip: bool,
}

impl CItemStack {
    /// 空栈。
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            item: None,
            count: 0,
            enchantments: Vec::new(),
            show_in_tooltip: true,
        }
    }

    /// 构造指定物品（无组件）。
    #[must_use]
    pub fn of(item: &str, count: i32) -> Self {
        Self {
            item: Some(item.to_owned()),
            count,
            enchantments: Vec::new(),
            show_in_tooltip: true,
        }
    }

    /// 是否为栈空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.item.is_none()
    }

    /// 编码载荷（`ItemStack.OPTIONAL_PACKET_CODEC`）。
    pub fn encode(&self, writer: &mut Writer) {
        let Some(item) = &self.item else {
            writer.write_var_int(0);
            return;
        };
        writer.write_var_int(self.count);
        // registryEntry：raw_id + 1（直接条目用 0，本实现不使用）
        let id = crate::item_ids::item_id(item).expect("unregistered item");
        writer.write_var_int(id + 1);
        // ComponentChanges：无组件时写 (0, 0)；有附魔时写 (1, 0) + enchantments
        if self.enchantments.is_empty() {
            writer.write_var_int(0);
            writer.write_var_int(0);
        } else {
            writer.write_var_int(1);
            writer.write_var_int(0);
            // `minecraft:enchantments` 组件注册序（DataComponentTypes.java 第 10 个
            // register，网络 ID = 9）
            writer.write_var_int(9);
            encode_enchantments(writer, &self.enchantments, self.show_in_tooltip);
        }
    }
}

/// 编码 `ItemEnchantmentsComponent.PACKET_CODEC`：`map(VarInt 条数, {ENTRY, VarInt})`
/// + `Bool showInTooltip`（对照 `ItemEnchantmentsComponent.java`）。
fn encode_enchantments(writer: &mut Writer, enchantments: &[(String, i32)], show: bool) {
    writer.write_var_int(i32::try_from(enchantments.len()).expect("enchantments exceed i32"));
    for (name, level) in enchantments {
        // Enchantment.ENTRY_PACKET_CODEC = registryEntry：raw_id + 1
        let id = crate::enchantment_ids::enchantment_id(name).expect("unregistered enchantment");
        writer.write_var_int(id + 1);
        writer.write_var_int(*level);
    }
    writer.write_bool(show);
}

/// Clientbound：单方块更新（0x09 `BlockUpdateS2CPacket`）。
///
/// 字段：`BlockPos` → `VarInt state_id`（对照 `BlockUpdateS2CPacket.java`：
/// `BlockPos.PACKET_CODEC` + `PacketCodecs.entryOf(Block.STATE_IDS)`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CBlockUpdate {
    /// 方块位置。
    pub position: BlockPos,
    /// 新方块状态 ID。
    pub state_id: i32,
}

impl CBlockUpdate {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i64_be(self.position.as_long());
        writer.write_var_int(self.state_id);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_BLOCK_UPDATE
    }
}

/// Clientbound：区块内多方块更新（0x49 `ChunkDeltaUpdateS2CPacket`）。
///
/// 字段：`i64 section 位置（x<<42 | z<<20 | y）` → `VarInt 数量` →
/// 每项 `VarLong(state_id << 12 | 区块内相对位置)`，相对位置 = `(y<<8|z<<4|x)`
/// 的 12 位（对照 `ChunkDeltaUpdateS2CPacket.java` 与 master `CMultiBlockUpdate`）。
#[derive(Debug, Clone, Default)]
pub struct CSectionBlocksUpdate {
    /// 区块 section 位置（方块坐标）：x/z 为区块坐标（16 方块）、y 为 section 索引。
    pub section: BlockPos,
    /// 更新列表（位置 + 新状态 ID）。
    pub updates: Vec<(BlockPos, i32)>,
}

impl CSectionBlocksUpdate {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        let x = self.section.x as i64 & 0x3FF_FFFF;
        let z = self.section.z as i64 & 0x3FF_FFFF;
        let y = i64::from(self.section.y) & 0xFFF;
        writer.write_i64_be((x << 42) | (z << 20) | y);
        writer.write_var_int(
            i32::try_from(self.updates.len()).expect("updates exceed i32"),
        );
        for (pos, state) in &self.updates {
            let local_x = pos.x & 0xF;
            let local_y = pos.y & 0xF;
            let local_z = pos.z & 0xF;
            let relative = ((local_y << 8) | (local_z << 4) | local_x) as i64;
            writer.write_var_long((i64::from(*state) << 12) | relative);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SECTION_BLOCKS_UPDATE
    }
}

/// 声音分类（`SoundCategory.java` 枚举序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum SoundCategory {
    Master = 0,
    Music = 1,
    Records = 2,
    Weather = 3,
    Blocks = 4,
    Hostile = 5,
    Neutral = 6,
    Players = 7,
    Ambient = 8,
    Voice = 9,
}

/// Clientbound：世界声音（0x68 `PlaySoundS2CPacket`）。
///
/// 字段：`SoundEvent`（registryEntry：VarInt id+1）→ `VarInt category` →
/// `i32 x*8/y*8/z*8` → `f32 volume` → `f32 pitch` → `i64 seed`（对照
/// `PlaySoundS2CPacket.java`，`COORDINATE_SCALE = 8.0`）。
#[derive(Debug, Clone)]
pub struct CPlaySound {
    /// 声音事件名（`minecraft:` 前缀省略，如 `entity.cow.ambient`）。
    pub sound: String,
    /// 分类。
    pub category: SoundCategory,
    /// 位置（方块）。
    pub x: f64,
    /// 位置。
    pub y: f64,
    /// 位置。
    pub z: f64,
    /// 音量。
    pub volume: f32,
    /// 音调。
    pub pitch: f32,
    /// 随机种子。
    pub seed: i64,
}

impl CPlaySound {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        let id = crate::sound_events::sound_id(&self.sound).expect("unregistered sound");
        writer.write_var_int(id + 1);
        writer.write_var_int(self.category as i32);
        writer.write_i32_be((self.x * 8.0) as i32);
        writer.write_i32_be((self.y * 8.0) as i32);
        writer.write_i32_be((self.z * 8.0) as i32);
        writer.write_f32_be(self.volume);
        writer.write_f32_be(self.pitch);
        writer.write_i64_be(self.seed);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SOUND
    }
}

/// Clientbound：实体声音（0x67 `PlaySoundFromEntityS2CPacket`）。
///
/// 字段：`SoundEvent` → `VarInt category` → `VarInt entity_id` →
/// `f32 volume` → `f32 pitch` → `i64 seed`（对照 `PlaySoundFromEntityS2CPacket.java`）。
#[derive(Debug, Clone)]
pub struct CEntitySound {
    /// 声音事件名。
    pub sound: String,
    /// 分类。
    pub category: SoundCategory,
    /// 实体 ID。
    pub entity_id: i32,
    /// 音量。
    pub volume: f32,
    /// 音调。
    pub pitch: f32,
    /// 随机种子。
    pub seed: i64,
}

impl CEntitySound {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        let id = crate::sound_events::sound_id(&self.sound).expect("unregistered sound");
        writer.write_var_int(id + 1);
        writer.write_var_int(self.category as i32);
        writer.write_var_int(self.entity_id);
        writer.write_f32_be(self.volume);
        writer.write_f32_be(self.pitch);
        writer.write_i64_be(self.seed);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SOUND_ENTITY
    }
}

/// Clientbound：槽位更新（0x15 `ScreenHandlerSlotUpdateS2CPacket`）。
///
/// 字段：`i8 sync_id` → `VarInt revision` → `i16 slot` → `ItemStack`（对照
/// `ScreenHandlerSlotUpdateS2CPacket.java`）。玩家物品栏  = -2
/// （`UPDATE_PLAYER_INVENTORY_SYNC_ID`）。
#[derive(Debug, Clone)]
pub struct CSetSlot {
    /// 容器窗口 ID（-2 = 玩家物品栏）。
    pub sync_id: i8,
    /// 修订号（客户端忽略大部分场景）。
    pub revision: i32,
    /// 槽位。
    pub slot: i16,
    /// 物品。
    pub stack: CItemStack,
}

impl CSetSlot {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i8(self.sync_id);
        writer.write_var_int(self.revision);
        writer.write_i16_be(self.slot);
        self.stack.encode(writer);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_CONTAINER_SET_SLOT
    }
}

/// Clientbound：实体动画（0x03 `EntityAnimationS2CPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct CEntityAnimation {
    /// 实体 ID。
    pub entity_id: i32,
    /// 动画 ID。
    pub animation: u8,
}

impl CEntityAnimation {
    /// 挥主手。
    pub const ANIMATION_SWING_MAIN_ARM: u8 = 0;
    /// 受击。
    pub const ANIMATION_HURT: u8 = 1;

    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.entity_id);
        writer.write_u8(self.animation);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_ANIMATE
    }
}

/// Clientbound：实体状态/事件（0x1F `EntityStatusS2CPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct CEntityStatus {
    /// 实体 ID。
    pub entity_id: i32,
    /// 状态 ID。
    pub status: i8,
}

impl CEntityStatus {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i32_be(self.entity_id);
        writer.write_i8(self.status);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_ENTITY_EVENT
    }
}

/// Clientbound：设置实体速度（0x5A `EntityVelocityUpdateS2CPacket`）。
///
/// 字段：`VarInt entity_id` → `i16 vx/vy/vz`（单位 1/8000 方块/秒，对照
/// `EntityVelocityUpdateS2CPacket.java` 的 `(int)(velocity * 8000)`）。
#[derive(Debug, Clone, Copy)]
pub struct CEntityVelocity {
    /// 实体 ID。
    pub entity_id: i32,
    /// X 速度（方块/秒）。
    pub vx: f64,
    /// Y 速度。
    pub vy: f64,
    /// Z 速度。
    pub vz: f64,
}

impl CEntityVelocity {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.entity_id);
        writer.write_i16_be(velocity_component(self.vx));
        writer.write_i16_be(velocity_component(self.vy));
        writer.write_i16_be(velocity_component(self.vz));
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_ENTITY_MOTION
    }
}

/// 速度分量 → 网络 i16（1/8000 方块/秒，钳制 ±3.9）。
#[must_use]
pub fn velocity_component(velocity: f64) -> i16 {
    (velocity.clamp(-3.9, 3.9) * 8000.0) as i16
}

/// Clientbound：设置经验（0x5C `ExperienceBarUpdateS2CPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct CSetExperience {
    /// 经验条进度（0..1）。
    pub progress: f32,
    /// 等级。
    pub level: i32,
    /// 总经验。
    pub total_experience: i32,
}

impl CSetExperience {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_f32_be(self.progress);
        writer.write_var_int(self.level);
        writer.write_var_int(self.total_experience);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_EXPERIENCE
    }
}

/// Clientbound：设置生命/饥饿（0x5D `HealthUpdateS2CPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct CSetHealth {
    /// 生命值。
    pub health: f32,
    /// 饥饿值。
    pub food: i32,
    /// 饱和度。
    pub saturation: f32,
}

impl CSetHealth {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_f32_be(self.health);
        writer.write_var_int(self.food);
        writer.write_f32_be(self.saturation);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_HEALTH
    }
}

/// Clientbound：世界时间（0x64 `WorldTimeUpdateS2CPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct CUpdateTime {
    /// 世界年龄（tick）。
    pub game_time: i64,
    /// 昼夜时间（tick；负数冻结昼夜循环）。
    pub day_time: i64,
}

impl CUpdateTime {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i64_be(self.game_time);
        writer.write_i64_be(self.day_time);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SET_TIME
    }
}

/// Clientbound：经验球生成（0x02 `ExperienceOrbSpawnS2CPacket`）。
#[derive(Debug, Clone, Copy)]
pub struct CAddExperienceOrb {
    /// 实体 ID。
    pub entity_id: i32,
    /// X。
    pub x: f64,
    /// Y。
    pub y: f64,
    /// Z。
    pub z: f64,
    /// 经验值。
    pub count: i16,
}

impl CAddExperienceOrb {
    /// 编码载荷（对照 `ExperienceOrbSpawnS2CPacket.java`：entityId → x/y/z → count）。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.entity_id);
        writer.write_f64_be(self.x);
        writer.write_f64_be(self.y);
        writer.write_f64_be(self.z);
        writer.write_i16_be(self.count);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_ADD_EXPERIENCE_ORB
    }
}

/// 爆炸破坏类型（`Explosion.DestructionType` 枚举序，VarInt）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExplosionDestructionType {
    Keep = 0,
    Destroy = 1,
    DestroyWithDecay = 2,
    TriggerBlock = 3,
}

/// Clientbound：爆炸（0x20 `ExplosionS2CPacket`）。
///
/// 字段（对照 `ExplosionS2CPacket.java`）：`f64 x/y/z` → `f32 radius` →
/// `VarInt 受影响方块数` + 每项 `i8` 相对中心偏移 → `f32` 玩家速度 ×3 →
/// `VarInt` 破坏类型 → 粒子 ×2（`ParticleTypes.PACKET_CODEC`，简单粒子 = registryEntry
/// `VarInt id+1`）→ `SoundEvent`（registryEntry `VarInt id+1`）。
#[derive(Debug, Clone)]
pub struct CExplosion {
    /// 爆炸中心。
    pub x: f64,
    /// 爆炸中心。
    pub y: f64,
    /// 爆炸中心。
    pub z: f64,
    /// 半径。
    pub radius: f32,
    /// 受影响方块（破坏区域，绝对坐标）。
    pub affected_blocks: Vec<BlockPos>,
    /// 玩家速度（方块/秒，爆炸推力）。
    pub player_velocity: [f32; 3],
    /// 破坏类型。
    pub destruction_type: ExplosionDestructionType,
    /// 粒子（`explosion_emitter`/`explosion` 等，registryEntry id+1）。
    pub particle: String,
    /// 发射粒子。
    pub emitter_particle: String,
    /// 爆炸声音事件。
    pub sound: String,
}

impl CExplosion {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_f64_be(self.x);
        writer.write_f64_be(self.y);
        writer.write_f64_be(self.z);
        writer.write_f32_be(self.radius);
        let cx = self.x.floor() as i64;
        let cy = self.y.floor() as i64;
        let cz = self.z.floor() as i64;
        writer.write_var_int(
            i32::try_from(self.affected_blocks.len()).expect("affected blocks exceed i32"),
        );
        for pos in &self.affected_blocks {
            writer.write_i8((i64::from(pos.x) - cx) as i8);
            writer.write_i8((i64::from(pos.y) - cy) as i8);
            writer.write_i8((i64::from(pos.z) - cz) as i8);
        }
        writer.write_f32_be(self.player_velocity[0]);
        writer.write_f32_be(self.player_velocity[1]);
        writer.write_f32_be(self.player_velocity[2]);
        writer.write_var_int(self.destruction_type as i32);
        let particle_id = crate::registry::particle_id(&self.particle).expect("unregistered particle");
        writer.write_var_int(particle_id + 1);
        let emitter_id =
            crate::registry::particle_id(&self.emitter_particle).expect("unregistered particle");
        writer.write_var_int(emitter_id + 1);
        let sound_id = crate::sound_events::sound_id(&self.sound).expect("unregistered sound");
        writer.write_var_int(sound_id + 1);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_EXPLOSION
    }
}

/// Clientbound：玩家重生/维度切换（0x47 `PlayerRespawnS2CPacket`）。
///
/// 字段：`CommonPlayerSpawnInfo`（与 `CLogin` 的 `spawn_data` 同构，编码见
/// `CommonPlayerSpawnInfo.java`）→ `i8 flag`（`KEEP_ATTRIBUTES`=1/`KEEP_TRACKED_DATA`=2）。
#[derive(Debug, Clone)]
pub struct CRespawn {
    /// 出生信息（维度、模式、种子等）。
    pub spawn_data: PlayerSpawnData,
    /// 保留数据标志（`KEEP_ATTRIBUTES` | `KEEP_TRACKED_DATA`）。
    pub data_kept: i8,
}

impl CRespawn {
    /// 保留属性（速度等）。
    pub const KEEP_ATTRIBUTES: i8 = 1;
    /// 保留实体数据（背包等）。
    pub const KEEP_TRACKED_DATA: i8 = 2;

    /// 编码载荷（`CommonPlayerSpawnInfo.write` 字段顺序同 `CLogin` 的 `spawn_data`）。
    pub fn encode(&self, writer: &mut Writer) {
        let s = &self.spawn_data;
        writer.write_var_int(s.dimension_type + 1);
        writer.write_string(&s.dimension_name);
        writer.write_i64_be(s.hashed_seed);
        writer.write_u8(s.game_mode);
        writer.write_i8(s.previous_gamemode);
        writer.write_bool(s.is_debug);
        writer.write_bool(s.is_flat);
        match &s.death_dimension {
            Some((name, pos)) => {
                writer.write_bool(true);
                writer.write_string(name);
                writer.write_i64_be(pos.as_long());
            }
            None => writer.write_bool(false),
        }
        writer.write_var_int(s.portal_cooldown);
        writer.write_i8(self.data_kept);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_RESPAWN
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Reader;
    use crate::nbt::Nbt;
    use crate::packets::play::{HEIGHTMAP_LONGS, block_state_ids};

    #[test]
    fn system_chat_encodes_nbt_component() {
        // 1.21.1 `GameMessageS2CPacket`：内容为 NBT 组件（`TextCodecs`），
        // 客户端 `readNbt` 解析；裸字符串会让首字节被当成 NBT 标签 ID。
        let packet = CSystemChat {
            content: "hello 世界".to_owned(),
            overlay: false,
        };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let nbt = r.read_nbt().unwrap().unwrap();
        let compound = match nbt {
            Nbt::Compound(c) => c,
            other => panic!("expected compound {other:?}"),
        };
        assert_eq!(compound.get_string("text"), Some("hello 世界"));
        assert!(!r.read_bool().unwrap());
        assert!(r.is_empty());
    }

    #[test]
    fn chunk_data_encodes_flat_section() {
        let mut sections = Vec::new();
        // 24 个 section：底部 3 个含方块（bedrock/dirt/grass），其余空气
        for section_y in 0..24 {
            if section_y == 0 {
                let mut blocks = [0i32; 4096];
                blocks.fill(block_state_ids::BEDROCK);
                sections.push(ChunkSection::from_block_ids(
                    &blocks,
                    flat_biomes::OVERWORLD,
                ));
            } else if section_y == 1 || section_y == 2 {
                let mut blocks = [0i32; 4096];
                blocks.fill(block_state_ids::DIRT);
                sections.push(ChunkSection::from_block_ids(
                    &blocks,
                    flat_biomes::OVERWORLD,
                ));
            } else if section_y == 3 {
                // 顶层 y=3 为草方块（world y = 3*16+3 = 51，本 flat 定义 y=3 即顶）
                let mut blocks = [0i32; 4096];
                for y in 0..16 {
                    for z in 0..16 {
                        for x in 0..16 {
                            if y == 3 {
                                blocks[(y << 4 | z) << 4 | x] = block_state_ids::GRASS_BLOCK;
                            }
                        }
                    }
                }
                sections.push(ChunkSection::from_block_ids(
                    &blocks,
                    flat_biomes::OVERWORLD,
                ));
            } else {
                sections.push(ChunkSection::air(flat_biomes::OVERWORLD));
            }
        }
        let heights = [4i32; 256];
        let packet = CLevelChunkWithLight::flat(0, 0, -64, heights, sections);
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_i32_be().unwrap(), 0);
        assert_eq!(r.read_i32_be().unwrap(), 0);
        let heightmap = r.read_nbt().unwrap().unwrap();
        let compound = match heightmap {
            Nbt::Compound(c) => c,
            other => panic!("expected compound {other:?}"),
        };
        assert_eq!(
            compound.get_long_array("MOTION_BLOCKING").unwrap().len(),
            HEIGHTMAP_LONGS
        );
        assert_eq!(
            compound.get_long_array("WORLD_SURFACE").unwrap().len(),
            HEIGHTMAP_LONGS
        );
        // 24 sections 的 blocks_and_biomes 长度
        let data_len = r.read_var_int().unwrap() as usize;
        let data = r.read_bytes(data_len).unwrap();
        assert_eq!(data.len(), data_len);
        assert_eq!(r.read_var_int().unwrap(), 0); // 无方块实体
        // 光照：vanilla `LightData` 约定掩码覆盖 n+2 个 section——bit 0 = 世界
        // 下方，bit 1..=24 = 世界 section，bit 25 = 世界上方；flat 世界 24 个
        // section 全部有天空光 → sky 掩码位 1..=24，block 空掩码位 0..=25。
        let sky_mask = r.read_bitset().unwrap();
        assert_eq!(sky_mask[0], 0x1FF_FFFE); // 位 1..=24
        let block_mask = r.read_bitset().unwrap();
        assert_eq!(block_mask[0], 0);
        let sky_empty = r.read_bitset().unwrap();
        assert_eq!(sky_empty[0], 0x200_0001); // 位 0 与位 25
        let block_empty = r.read_bitset().unwrap();
        assert_eq!(block_empty[0], 0x3FF_FFFF); // 位 0..=25
    }

    #[test]
    fn command_tree_encodes_literals_and_arguments() {
        // 对照 `CommandTreeS2CPacket.write`：VarInt 节点数 → 每节点 flags(Byte) +
        // 子节点 VarInt 数组 → literal/argument 数据 → 末尾 rootIndex。
        let packet = CCommands {
            nodes: vec![
                CommandTreeNode::Root {
                    children: vec![1, 2],
                },
                CommandTreeNode::Literal {
                    name: "gamemode".to_owned(),
                    children: vec![2],
                },
                CommandTreeNode::Argument {
                    name: "mode".to_owned(),
                    children: vec![],
                },
            ],
        };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_var_int().unwrap(), 3); // 节点数
        // 根节点：flags=0，children=[1,2]
        assert_eq!(r.read_u8().unwrap(), 0);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_var_int().unwrap(), 2);
        // 字面量：flags=1，children=[2]，name="gamemode"
        assert_eq!(r.read_u8().unwrap(), 1);
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_string().unwrap(), "gamemode");
        // 参数：flags=2，children=[]，name="mode"，brigadier:string(5) + word(0)
        assert_eq!(r.read_u8().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.read_string().unwrap(), "mode");
        assert_eq!(r.read_var_int().unwrap(), 5); // brigadier:string
        assert_eq!(r.read_var_int().unwrap(), 0); // SINGLE_WORD
        assert_eq!(r.read_var_int().unwrap(), 0); // rootIndex 在末尾
        assert!(r.is_empty());
    }

    #[test]
    fn command_suggestions_encodes_entries() {
        let packet = CCommandSuggestions {
            id: 9,
            start: 10,
            length: 2,
            suggestions: vec!["creative".to_owned(), "survival".to_owned()],
        };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_var_int().unwrap(), 9);
        assert_eq!(r.read_var_int().unwrap(), 10);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), 2); // 建议数
        assert_eq!(r.read_string().unwrap(), "creative");
        assert!(!r.read_bool().unwrap()); // 无 tooltip
        assert_eq!(r.read_string().unwrap(), "survival");
        assert!(!r.read_bool().unwrap());
        assert!(r.is_empty());
    }

    #[test]
    fn join_game_encoding() {
        let packet = CLogin {
            entity_id: 1,
            is_hardcore: false,
            dimension_names: vec![
                "minecraft:overworld".to_owned(),
                "minecraft:the_nether".to_owned(),
                "minecraft:the_end".to_owned(),
            ],
            max_players: 20,
            view_distance: 10,
            simulation_distance: 8,
            reduced_debug_info: false,
            enable_respawn_screen: true,
            limited_crafting: false,
            enforces_secure_chat: false,
            spawn_data: PlayerSpawnData {
                dimension_type: 0,
                dimension_name: "minecraft:overworld".to_owned(),
                hashed_seed: 0,
                game_mode: 0,
                previous_gamemode: -1,
                is_debug: false,
                is_flat: true,
                death_dimension: None,
                portal_cooldown: 0,
            },
        };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_i32_be().unwrap(), 1);
        assert!(!r.read_bool().unwrap());
        assert_eq!(r.read_var_int().unwrap(), 3);
        assert_eq!(r.read_string().unwrap(), "minecraft:overworld");
        assert_eq!(r.read_string().unwrap(), "minecraft:the_nether");
        assert_eq!(r.read_string().unwrap(), "minecraft:the_end");
        assert_eq!(r.read_var_int().unwrap(), 20);
        assert_eq!(r.read_var_int().unwrap(), 10);
        assert_eq!(r.read_var_int().unwrap(), 8);
        assert!(!r.read_bool().unwrap()); // reduced_debug_info
        assert!(r.read_bool().unwrap()); // enable_respawn_screen
        assert!(!r.read_bool().unwrap()); // limited_crafting
    }
}
