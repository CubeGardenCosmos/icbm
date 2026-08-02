//! 配置阶段包。

use crate::codec::{CodecError, Reader, Writer};
use crate::nbt::{Nbt, NbtCompound};

use super::config_ids::{
    CLIENTBOUND_CUSTOM_PAYLOAD, CLIENTBOUND_DISCONNECT, CLIENTBOUND_FINISH_CONFIGURATION,
    CLIENTBOUND_KEEP_ALIVE, CLIENTBOUND_KNOWN_PACKS, CLIENTBOUND_PING, CLIENTBOUND_REGISTRY_DATA,
    CLIENTBOUND_UPDATE_ENABLED_FEATURES, CLIENTBOUND_UPDATE_TAGS, SERVERBOUND_CLIENT_INFORMATION,
    SERVERBOUND_CUSTOM_PAYLOAD, SERVERBOUND_FINISH_CONFIGURATION, SERVERBOUND_KEEP_ALIVE,
    SERVERBOUND_KNOWN_PACKS, SERVERBOUND_PONG,
};

/// Serverbound：客户端信息（0x00）。
#[derive(Debug, Clone)]
pub struct SClientInformation {
    /// 语言标签（如 `zh_cn`）。
    pub locale: String,
    /// 可视距离（区块）。
    pub view_distance: i8,
    /// 聊天模式（0=启用，1=命令，2=隐藏）。
    pub chat_mode: i32,
    /// 是否显示聊天颜色。
    pub chat_colors: bool,
    /// 皮肤部件位掩码。
    pub skin_parts: u8,
    /// 主手（0=右手，1=左手）。
    pub main_hand: i32,
    /// 是否启用文本过滤。
    pub enable_text_filtering: bool,
    /// 是否允许列入服务器列表。
    pub allow_server_listing: bool,
}

impl SClientInformation {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            locale: reader.read_string()?,
            view_distance: reader.read_i8()?,
            chat_mode: i32::from(reader.read_u8()?),
            chat_colors: reader.read_bool()?,
            skin_parts: reader.read_u8()?,
            main_hand: i32::from(reader.read_u8()?),
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

/// 资源包标识（对应官方 `VersionedIdentifier`：namespace/id/version 三个字符串）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownPack {
    /// 命名空间（如 `minecraft`）。
    pub namespace: String,
    /// 包 ID（如 `core`）。
    pub id: String,
    /// 包版本（如 `1.21.1`）。
    pub version: String,
}

/// Serverbound：已知资源包列表（0x07）。
#[derive(Debug, Clone, Default)]
pub struct SKnownPacks {
    /// 客户端已拥有的资源包。
    pub packs: Vec<KnownPack>,
}

impl SKnownPacks {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let count = reader.read_var_int()?;
        let mut packs = Vec::with_capacity(usize::try_from(count).unwrap_or_default());
        for _ in 0..count {
            packs.push(KnownPack {
                namespace: reader.read_string()?,
                id: reader.read_string()?,
                version: reader.read_string()?,
            });
        }
        Ok(Self { packs })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_KNOWN_PACKS
    }
}

/// Serverbound：完成配置（0x03，空载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SFinishConfig;

impl SFinishConfig {
    /// 解码（空载荷）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        debug_assert!(reader.is_empty());
        Ok(Self)
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_FINISH_CONFIGURATION
    }
}

/// Serverbound：配置阶段插件消息（0x02）。
#[derive(Debug, Clone)]
pub struct SPluginMessage {
    /// 通道标识（如 `minecraft:brand`）。
    pub channel: String,
    /// 通道数据。
    pub data: Vec<u8>,
}

impl SPluginMessage {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            channel: reader.read_string()?,
            data: reader.read_rest().to_vec(),
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_CUSTOM_PAYLOAD
    }
}

/// Serverbound：配置阶段存活确认（0x04）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SKeepAlive {
    /// 服务端发送的存活 ID。
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

/// Serverbound：配置阶段 pong（0x05）。
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

/// Clientbound：配置阶段插件消息（0x01）。
#[derive(Debug, Clone)]
pub struct CPluginMessage {
    /// 通道标识。
    pub channel: String,
    /// 通道数据。
    pub data: Vec<u8>,
}

impl CPluginMessage {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_string(&self.channel);
        writer.write_slice(&self.data);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_CUSTOM_PAYLOAD
    }
}

/// Clientbound：配置阶段断开（0x02）。
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

/// Clientbound：完成配置（0x03，空载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CFinishConfig;

impl CFinishConfig {
    /// 编码载荷。
    #[allow(clippy::unused_self, clippy::missing_const_for_fn)]
    pub const fn encode(&self, _writer: &mut Writer) {}

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_FINISH_CONFIGURATION
    }
}

/// Clientbound：配置阶段存活检测（0x04）。
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

/// Clientbound：配置阶段 ping（0x05）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CPing {
    /// ping ID，客户端须原样回显。
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

/// 注册表条目。
#[derive(Debug, Clone)]
pub struct RegistryEntry {
    /// 条目 ID（如 `minecraft:overworld`）。
    pub id: String,
    /// 条目 NBT 数据（可为空复合）。
    pub data: Nbt,
}

/// Clientbound：注册表数据（0x07）。
#[derive(Debug, Clone)]
pub struct CRegistryData {
    /// 注册表 ID（如 `minecraft:dimension_type`）。
    pub registry_id: String,
    /// 条目列表（顺序即网络 ID）。
    pub entries: Vec<RegistryEntry>,
}

impl CRegistryData {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_string(&self.registry_id);
        writer
            .write_var_int(i32::try_from(self.entries.len()).expect("registry entries exceed i32"));
        for entry in &self.entries {
            writer.write_string(&entry.id);
            writer.write_bool(true);
            writer
                .write_nbt_unnamed(&entry.data)
                .expect("registry nbt must encode");
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_REGISTRY_DATA
    }
}

/// 注册表名 → （标签名 → 条目 ID 列表）。
pub type RegistryTags = Vec<(String, Vec<(String, Vec<i32>)>)>;

/// 标签条目（注册表名 → 标签名 → 条目 ID）。
#[derive(Debug, Clone, Default)]
pub struct Tags {
    /// 注册表名 → （标签名 → 条目 ID 列表）。
    pub registries: RegistryTags,
}

impl Tags {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(
            i32::try_from(self.registries.len()).expect("tag registries exceed i32"),
        );
        for (registry, tags) in &self.registries {
            writer.write_string(registry);
            writer.write_var_int(i32::try_from(tags.len()).expect("tags exceed i32"));
            for (name, entries) in tags {
                writer.write_string(name);
                writer.write_var_int(i32::try_from(entries.len()).expect("tag entries exceed i32"));
                for entry in entries {
                    writer.write_var_int(*entry);
                }
            }
        }
    }
}

/// Clientbound：更新标签（0x0D）。
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

/// Clientbound：启用的特性标志（0x0C）。
#[derive(Debug, Clone, Default)]
pub struct CUpdateEnabledFeatures {
    /// 特性标识（如 `minecraft:vanilla`）。
    pub features: Vec<String>,
}

impl CUpdateEnabledFeatures {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(i32::try_from(self.features.len()).expect("features exceed i32"));
        for feature in &self.features {
            writer.write_string(feature);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_UPDATE_ENABLED_FEATURES
    }
}

/// Clientbound：已知资源包列表（0x0E）。
#[derive(Debug, Clone, Default)]
pub struct CKnownPacks {
    /// 服务器要求客户端使用的资源包。
    pub packs: Vec<KnownPack>,
}

impl CKnownPacks {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(i32::try_from(self.packs.len()).expect("packs exceed i32"));
        for pack in &self.packs {
            writer.write_string(&pack.namespace);
            writer.write_string(&pack.id);
            writer.write_string(&pack.version);
        }
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_KNOWN_PACKS
    }
}

/// 构造空 NBT 复合的便捷函数。
#[must_use]
pub fn empty_compound() -> Nbt {
    Nbt::Compound(Box::new(NbtCompound::new()))
}

#[cfg(test)]
mod tests {
    use super::{CRegistryData, CUpdateTags, RegistryEntry, SKnownPacks, Tags, empty_compound};
    use crate::codec::{Reader, Writer};
    use crate::nbt::{Nbt, NbtCompound};

    #[test]
    fn registry_data_round_trip() {
        let mut effects = NbtCompound::new();
        effects.insert("sky_color", Nbt::Int(0x78A7FF));
        let mut biome = NbtCompound::new();
        biome.insert("has_precipitation", Nbt::Byte(1));
        biome.insert("temperature", Nbt::Float(0.8));
        biome.insert("downfall", Nbt::Float(0.4));
        biome.insert("effects", Nbt::Compound(Box::new(effects)));

        let packet = CRegistryData {
            registry_id: "minecraft:worldgen/biome".to_owned(),
            entries: vec![RegistryEntry {
                id: "minecraft:plains".to_owned(),
                data: Nbt::Compound(Box::new(biome)),
            }],
        };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_string().unwrap(), "minecraft:worldgen/biome");
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_string().unwrap(), "minecraft:plains");
        assert!(r.read_bool().unwrap());
        let nbt = r.read_nbt().unwrap().unwrap();
        let compound = match nbt {
            Nbt::Compound(c) => c,
            other => panic!("expected compound, got {other:?}"),
        };
        assert_eq!(compound.get_bool("has_precipitation"), Some(true));
        assert_eq!(
            compound
                .get_compound("effects")
                .unwrap()
                .get_i32("sky_color"),
            Some(0x78A7FF)
        );
        assert!(r.is_empty());
    }

    #[test]
    fn tags_round_trip() {
        let tags = Tags {
            registries: vec![(
                "minecraft:block".to_owned(),
                vec![("minecraft:climbable".to_owned(), vec![1, 2, 3])],
            )],
        };
        let packet = CUpdateTags { tags };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_string().unwrap(), "minecraft:block");
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_string().unwrap(), "minecraft:climbable");
        assert_eq!(r.read_var_int().unwrap(), 3);
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), 3);
    }

    #[test]
    fn known_packs_round_trip() {
        let mut w = Writer::new();
        w.write_var_int(1);
        w.write_string("minecraft");
        w.write_string("core");
        w.write_string("1.21.1");
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let packs = SKnownPacks::decode(&mut r).unwrap();
        assert_eq!(packs.packs.len(), 1);
        assert_eq!(packs.packs[0].namespace, "minecraft");
        assert_eq!(packs.packs[0].id, "core");
        assert_eq!(packs.packs[0].version, "1.21.1");
    }

    #[test]
    fn empty_compound_helper() {
        let nbt = empty_compound();
        assert!(matches!(nbt, Nbt::Compound(_)));
    }
}
