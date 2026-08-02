//! Play 阶段 clientbound 包。

use uuid::Uuid;

use crate::codec::Writer;
use crate::nbt::{Nbt, NbtCompound};
use crate::packets::config::Tags;
use crate::packets::play::{
    BlockPos, HEIGHTMAP_COLUMNS, encode_heightmap, encode_paletted_container,
};
use crate::packets::play_ids::{
    CLIENTBOUND_BUNDLE_DELIMITER, CLIENTBOUND_CHANGE_DIFFICULTY, CLIENTBOUND_CHUNK_BATCH_FINISHED,
    CLIENTBOUND_CHUNK_BATCH_START, CLIENTBOUND_COMMANDS, CLIENTBOUND_DISCONNECT,
    CLIENTBOUND_FORGET_LEVEL_CHUNK, CLIENTBOUND_GAME_EVENT, CLIENTBOUND_KEEP_ALIVE,
    CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT, CLIENTBOUND_LOGIN, CLIENTBOUND_PING,
    CLIENTBOUND_PLAYER_ABILITIES, CLIENTBOUND_PLAYER_INFO_REMOVE, CLIENTBOUND_PLAYER_INFO_UPDATE,
    CLIENTBOUND_PLAYER_POSITION, CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
    CLIENTBOUND_SET_CHUNK_CACHE_RADIUS, CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION,
    CLIENTBOUND_SYSTEM_CHAT, CLIENTBOUND_UPDATE_TAGS,
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

/// Clientbound：区块数据（0x27，含光照）。
#[derive(Debug, Clone)]
pub struct CLevelChunkWithLight {
    /// 区块 X。
    pub x: i32,
    /// 区块 Z。
    pub z: i32,
    /// 列高（256 列，值 = 最高匹配方块 y + 1）。
    pub world_surface_heights: [u16; HEIGHTMAP_COLUMNS],
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
        heights: [u16; HEIGHTMAP_COLUMNS],
        sections: Vec<ChunkSection>,
    ) -> Self {
        let full = vec![0xFFu8; 2048];
        let sky_light = vec![full; sections.len()];
        Self {
            x,
            z,
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
        let motion = encode_heightmap(&self.world_surface_heights);
        let mut heightmap = NbtCompound::new();
        heightmap.insert("WORLD_SURFACE", Nbt::LongArray(Box::new(motion)));
        let motion_blocking = encode_heightmap(&self.world_surface_heights);
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

        // 光照掩码
        let num_sections = self.sections.len();
        let mut sky_light_mask = 0i64;
        let mut sky_light_empty_mask = 0i64;
        let mut block_light_empty_mask = 0i64;

        sky_light_empty_mask |= 1 << 0;
        block_light_empty_mask |= 1 << 0;
        for section_index in 0..num_sections {
            let bit = section_index + 1;
            if section_index < self.sky_light.len() {
                sky_light_mask |= 1 << bit;
            } else {
                sky_light_empty_mask |= 1 << bit;
            }
            block_light_empty_mask |= 1 << bit;
        }
        sky_light_empty_mask |= 1 << (num_sections + 1);
        block_light_empty_mask |= 1 << (num_sections + 1);

        writer.write_bitset(&[sky_light_mask]);
        writer.write_bitset(&[0]); // block light mask 恒空
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
        writer.write_var_int(self.spawn_data.dimension_type);
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
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i32_be(self.batch_size);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_CHUNK_BATCH_FINISHED
    }
}

/// Clientbound：命令树（0x11，Phase 1 仅根节点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CCommands;

impl CCommands {
    /// 编码载荷（根节点：flags=0、无子节点）。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(1); // 节点数
        writer.write_var_int(0); // 根节点 flags
        writer.write_var_int(0); // 根节点子节点数
        writer.write_var_int(0); // 根索引
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_COMMANDS
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
    /// 区块加载开始事件。
    pub const EVENT_LEVEL_CHUNKS_LOAD_START: u8 = 12;
    /// 区块加载完成事件。
    pub const EVENT_LEVEL_CHUNKS_LOAD_DONE: u8 = 13;

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
#[derive(Debug, Clone)]
pub struct CSystemChat {
    /// 内容 JSON 文本组件。
    pub content: String,
    /// 是否覆盖模式（Action Bar）。
    pub overlay: bool,
}

impl CSystemChat {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_string(&self.content);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::Reader;
    use crate::packets::play::{HEIGHTMAP_LONGS, block_state_ids};

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
        let heights = [4u16; 256];
        let packet = CLevelChunkWithLight::flat(0, 0, heights, sections);
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
        // 光照：sky mask = bits 1..=24
        let sky_mask = r.read_bitset().unwrap();
        assert_eq!(sky_mask[0] & 0x1FF_FFFF, 0x1FF_FFFE);
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
