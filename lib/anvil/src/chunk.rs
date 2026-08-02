//! chunk NBT 编解码（对照 `ChunkSerializer.java`，`DataVersion` 3955）。
//!
//! 存储格式要点：
//! - 根复合命名 NBT（空名），顶层 `DataVersion` = 3955
//! - `sections`：每节 `Y`（节 y）、`block_states`/`biomes`（palette 列表 +
//!   可选打包 `data`）、`SkyLight`/`BlockLight`（2048 字节）
//! - palette 条目为 `{Name: "minecraft:xxx"}`；`data` 位数由 palette 大小按
//!   1.21.1 规则推断（blocks：2..16 → 4 位；biomes：2..8 → `ceil_log2`）

use std::fmt;

use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::nbt::{Nbt, NbtCompound, NbtError, NbtList, TagId, decode_named, encode_named};
use icbm_mc_767::packets::play::ceil_log2;
use icbm_mc_767::protocol::DATA_VERSION;

/// 区块数据读取错误。
#[derive(Debug)]
pub enum ChunkReadError {
    /// 不支持的 `DataVersion`。
    BadDataVersion(i32),
    /// 缺少必需字段。
    MissingField(&'static str),
    /// NBT 解析失败。
    Nbt(NbtError),
    /// 区块数据不完整。
    Truncated,
}

impl fmt::Display for ChunkReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadDataVersion(v) => {
                write!(f, "unsupported DataVersion {v} (expected {DATA_VERSION})")
            }
            Self::MissingField(field) => write!(f, "missing chunk field: {field}"),
            Self::Nbt(err) => write!(f, "nbt: {err}"),
            Self::Truncated => f.write_str("truncated chunk data"),
        }
    }
}

impl std::error::Error for ChunkReadError {}

impl From<NbtError> for ChunkReadError {
    fn from(value: NbtError) -> Self {
        Self::Nbt(value)
    }
}

/// 区块 section（存储视角：palette 名 + 打包数据 + 光照）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionNbt {
    /// 节 Y。
    pub y: i32,
    /// 方块 palette（存储名，如 `minecraft:air`）。
    pub block_palette: Vec<String>,
    /// 方块打包数据（单值 palette 时为空）。
    pub block_data: Vec<i64>,
    /// 生物群系 palette。
    pub biome_palette: Vec<String>,
    /// 生物群系打包数据。
    pub biome_data: Vec<i64>,
    /// 天空光（2048 字节）。
    pub sky_light: Option<Vec<u8>>,
    /// 方块光（2048 字节）。
    pub block_light: Option<Vec<u8>>,
}

/// 区块 NBT 数据模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkNbt {
    /// 区块 X。
    pub x: i32,
    /// 区块 Z。
    pub z: i32,
    /// 底部 section Y。
    pub bottom_section_y: i32,
    /// sections（自底向上）。
    pub sections: Vec<SectionNbt>,
    /// heightmaps（名 → 长整型数组）。
    pub heightmaps: Vec<(String, Vec<i64>)>,
    /// 世界时间。
    pub last_update: i64,
    /// 居住时间。
    pub inhabited_time: i64,
    /// 光照是否已计算。
    pub is_light_on: bool,
}

impl ChunkNbt {
    /// 新建区块（高度图与光照标志默认）。
    #[must_use]
    pub const fn new(x: i32, z: i32, bottom_section_y: i32, sections: Vec<SectionNbt>) -> Self {
        Self {
            x,
            z,
            bottom_section_y,
            sections,
            heightmaps: Vec::new(),
            last_update: 0,
            inhabited_time: 0,
            is_light_on: true,
        }
    }

    /// 序列化为命名 NBT 字节。
    pub fn to_bytes(&self) -> Result<Vec<u8>, ChunkReadError> {
        let mut writer = Writer::new();
        encode_named(&self.to_nbt(), &mut writer).map_err(ChunkReadError::Nbt)?;
        Ok(writer.into_bytes())
    }

    /// 从命名 NBT 字节解析。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ChunkReadError> {
        let mut reader = Reader::new(bytes);
        let nbt = decode_named(&mut reader).map_err(ChunkReadError::Nbt)?;
        if !reader.is_empty() {
            return Err(ChunkReadError::Truncated);
        }
        Self::from_nbt(nbt)
    }

    /// 构造存储 NBT（顶层 `DataVersion`）。
    #[must_use]
    pub fn to_nbt(&self) -> Nbt {
        let mut root = NbtCompound::new();
        root.insert("DataVersion", Nbt::Int(DATA_VERSION));
        root.insert("xPos", Nbt::Int(self.x));
        root.insert("yPos", Nbt::Int(self.bottom_section_y));
        root.insert("zPos", Nbt::Int(self.z));
        root.insert("LastUpdate", Nbt::Long(self.last_update));
        root.insert("InhabitedTime", Nbt::Long(self.inhabited_time));
        root.insert("Status", Nbt::String("minecraft:full".to_owned()));
        root.insert("isLightOn", Nbt::Byte(i8::from(self.is_light_on)));

        // sections
        let mut sections = NbtList::empty();
        sections.tag = TagId::Compound;
        for section in &self.sections {
            sections
                .values
                .push(Nbt::Compound(Box::new(section.to_nbt())));
        }
        root.insert("sections", Nbt::List(Box::new(sections)));

        // heightmaps
        let mut heightmaps = NbtCompound::new();
        for (name, data) in &self.heightmaps {
            heightmaps.insert(
                name.clone(),
                Nbt::LongArray(data.clone().into_boxed_slice()),
            );
        }
        root.insert("Heightmaps", Nbt::Compound(Box::new(heightmaps)));

        // 空结构
        let mut structures = NbtCompound::new();
        structures.insert("starts", Nbt::Compound(Box::new(NbtCompound::new())));
        structures.insert("references", Nbt::Compound(Box::new(NbtCompound::new())));
        root.insert("structures", Nbt::Compound(Box::new(structures)));
        root.insert("block_entities", Nbt::List(Box::new(NbtList::empty())));
        root.insert("entities", Nbt::List(Box::new(NbtList::empty())));
        root.insert("block_ticks", Nbt::List(Box::new(NbtList::empty())));
        root.insert("fluid_ticks", Nbt::List(Box::new(NbtList::empty())));

        Nbt::Compound(Box::new(root))
    }

    /// 从存储 NBT 解析。
    pub fn from_nbt(nbt: Nbt) -> Result<Self, ChunkReadError> {
        let Nbt::Compound(root) = nbt else {
            return Err(ChunkReadError::MissingField("root compound"));
        };

        if root.get_i32("DataVersion") != Some(DATA_VERSION) {
            return Err(ChunkReadError::BadDataVersion(
                root.get_i32("DataVersion").unwrap_or_default(),
            ));
        }

        let x = root
            .get_i32("xPos")
            .ok_or(ChunkReadError::MissingField("xPos"))?;
        let z = root
            .get_i32("zPos")
            .ok_or(ChunkReadError::MissingField("zPos"))?;
        let bottom_section_y = root
            .get_i32("yPos")
            .ok_or(ChunkReadError::MissingField("yPos"))?;

        let mut sections = Vec::new();
        if let Some(list) = root.get_list("sections") {
            for entry in &list.values {
                let Nbt::Compound(compound) = entry else {
                    return Err(ChunkReadError::MissingField("section compound"));
                };
                sections.push(SectionNbt::from_nbt(compound)?);
            }
        }
        sections.sort_by_key(|s| s.y);

        let mut heightmaps = Vec::new();
        if let Some(compound) = root.get_compound("Heightmaps") {
            for (name, data) in &compound.entries {
                if let Nbt::LongArray(values) = data {
                    heightmaps.push((name.clone(), values.to_vec()));
                }
            }
        }

        Ok(Self {
            x,
            z,
            bottom_section_y,
            sections,
            heightmaps,
            last_update: root.get_i64("LastUpdate").unwrap_or(0),
            inhabited_time: root.get_i64("InhabitedTime").unwrap_or(0),
            is_light_on: root.get_bool("isLightOn").unwrap_or(false),
        })
    }
}

impl SectionNbt {
    /// 构造存储 NBT。
    fn to_nbt(&self) -> NbtCompound {
        let mut section = NbtCompound::new();
        section.insert("Y", Nbt::Byte(i8::try_from(self.y).unwrap_or(0)));

        let mut block_states = NbtCompound::new();
        block_states.insert(
            "palette",
            Nbt::List(Box::new(palette_nbt(&self.block_palette))),
        );
        if !self.block_data.is_empty() {
            block_states.insert(
                "data",
                Nbt::LongArray(self.block_data.clone().into_boxed_slice()),
            );
        }
        section.insert("block_states", Nbt::Compound(Box::new(block_states)));

        let mut biomes = NbtCompound::new();
        biomes.insert(
            "palette",
            Nbt::List(Box::new(palette_nbt(&self.biome_palette))),
        );
        if !self.biome_data.is_empty() {
            biomes.insert(
                "data",
                Nbt::LongArray(self.biome_data.clone().into_boxed_slice()),
            );
        }
        section.insert("biomes", Nbt::Compound(Box::new(biomes)));

        if let Some(sky) = &self.sky_light {
            section.insert(
                "SkyLight",
                Nbt::ByteArray(sky.iter().map(|&b| b as i8).collect()),
            );
        }
        if let Some(block) = &self.block_light {
            section.insert(
                "BlockLight",
                Nbt::ByteArray(block.iter().map(|&b| b as i8).collect()),
            );
        }
        section
    }

    /// 从存储 NBT 解析。
    fn from_nbt(compound: &NbtCompound) -> Result<Self, ChunkReadError> {
        let y = compound
            .get_i32("Y")
            .ok_or(ChunkReadError::MissingField("section Y"))?;

        let block_states = compound
            .get_compound("block_states")
            .ok_or(ChunkReadError::MissingField("section block_states"))?;
        let (block_palette, block_data) = parse_container(block_states)?;

        let biomes = compound
            .get_compound("biomes")
            .ok_or(ChunkReadError::MissingField("section biomes"))?;
        let (biome_palette, biome_data) = parse_container(biomes)?;

        let sky_light = compound.get("SkyLight").and_then(byte_array);
        let block_light = compound.get("BlockLight").and_then(byte_array);

        Ok(Self {
            y,
            block_palette,
            block_data,
            biome_palette,
            biome_data,
            sky_light,
            block_light,
        })
    }

    /// 打包位数（blocks：2..16 → 4；biomes：2..8 → `ceil_log2`）。
    ///
    /// 与 vanilla `PalettedContainer.PaletteProvider.getBits` 一致。
    #[must_use]
    pub const fn block_bits(&self) -> u32 {
        match self.block_palette.len() {
            0 | 1 => 0,
            2..=16 => 4,
            17..=256 => ceil_log2(self.block_palette.len() as u32),
            _ => 15,
        }
    }

    /// 生物群系打包位数。
    #[must_use]
    pub const fn biome_bits(&self) -> u32 {
        match self.biome_palette.len() {
            0 | 1 => 0,
            2..=8 => ceil_log2(self.biome_palette.len() as u32),
            _ => 6,
        }
    }
}

/// 解析 `{palette, data?}` 容器。
fn parse_container(container: &NbtCompound) -> Result<(Vec<String>, Vec<i64>), ChunkReadError> {
    let palette_list = container
        .get_list("palette")
        .ok_or(ChunkReadError::MissingField("container palette"))?;
    let mut palette = Vec::with_capacity(palette_list.values.len());
    for entry in &palette_list.values {
        let Nbt::Compound(compound) = entry else {
            return Err(ChunkReadError::MissingField("palette entry compound"));
        };
        let name = compound
            .get_string("Name")
            .ok_or(ChunkReadError::MissingField("palette entry Name"))?;
        palette.push(name.to_owned());
    }
    let data = match container.get("data") {
        Some(Nbt::LongArray(values)) => values.to_vec(),
        Some(_) => return Err(ChunkReadError::MissingField("palette data long array")),
        None => Vec::new(),
    };
    Ok((palette, data))
}

fn byte_array(value: &Nbt) -> Option<Vec<u8>> {
    match value {
        Nbt::ByteArray(values) => Some(values.iter().map(|&b| b as u8).collect()),
        _ => None,
    }
}

fn palette_nbt(names: &[String]) -> NbtList {
    let mut list = NbtList::empty();
    list.tag = TagId::Compound;
    for name in names {
        let mut entry = NbtCompound::new();
        entry.insert("Name", Nbt::String(name.clone()));
        list.values.push(Nbt::Compound(Box::new(entry)));
    }
    list
}

#[cfg(test)]
mod tests {
    use super::{ChunkNbt, SectionNbt};
    use icbm_mc_767::packets::play::{HEIGHTMAP_LONGS, encode_heightmap};

    fn sample_chunk(x: i32, z: i32) -> ChunkNbt {
        let mut sections = Vec::new();
        for sy in -4..20 {
            let palette = match sy {
                -4 => vec!["minecraft:bedrock".to_owned()],
                -3 | -2 => vec!["minecraft:dirt".to_owned()],
                0 => vec![
                    "minecraft:air".to_owned(),
                    "minecraft:grass_block".to_owned(),
                ],
                _ => vec!["minecraft:air".to_owned()],
            };
            sections.push(SectionNbt {
                y: sy,
                block_palette: palette,
                block_data: if sy == 0 { vec![0i64; 256] } else { Vec::new() },
                biome_palette: vec!["minecraft:plains".to_owned()],
                biome_data: Vec::new(),
                sky_light: (sy >= 0).then(|| vec![0xFFu8; 2048]),
                block_light: None,
            });
        }
        let mut chunk = ChunkNbt::new(x, z, -4, sections);
        let heights = [4u16; 256];
        chunk.heightmaps.push((
            "WORLD_SURFACE".to_owned(),
            encode_heightmap(&heights).to_vec(),
        ));
        chunk.heightmaps.push((
            "MOTION_BLOCKING".to_owned(),
            encode_heightmap(&heights).to_vec(),
        ));
        chunk
    }

    #[test]
    fn chunk_nbt_round_trip() {
        let chunk = sample_chunk(3, -7);
        let bytes = chunk.to_bytes().unwrap();
        let parsed = ChunkNbt::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, chunk);
    }

    #[test]
    fn chunk_nbt_structure() {
        let chunk = sample_chunk(0, 0);
        let nbt = chunk.to_nbt();
        let root = match &nbt {
            icbm_mc_767::nbt::Nbt::Compound(c) => c,
            other => panic!("expected compound {other:?}"),
        };
        assert_eq!(root.get_i32("DataVersion"), Some(3955));
        assert_eq!(root.get_i32("yPos"), Some(-4));
        assert_eq!(root.get_string("Status"), Some("minecraft:full"));
        let sections = root.get_list("sections").unwrap();
        assert_eq!(sections.values.len(), 24);
        let heightmaps = root.get_compound("Heightmaps").unwrap();
        assert_eq!(
            heightmaps.get_long_array("MOTION_BLOCKING").unwrap().len(),
            HEIGHTMAP_LONGS
        );
    }

    #[test]
    fn wrong_data_version_rejected() {
        let chunk = sample_chunk(0, 0);
        let mut nbt = chunk.to_nbt();
        let root = match &mut nbt {
            icbm_mc_767::nbt::Nbt::Compound(c) => c,
            other => panic!("expected compound {other:?}"),
        };
        root.insert("DataVersion", icbm_mc_767::nbt::Nbt::Int(3465));
        let mut writer = icbm_mc_767::codec::Writer::new();
        icbm_mc_767::nbt::encode_named(&nbt, &mut writer).unwrap();
        let err = ChunkNbt::from_bytes(&writer.into_bytes()).unwrap_err();
        assert!(matches!(err, super::ChunkReadError::BadDataVersion(3465)));
    }

    #[test]
    fn section_bits_rules() {
        let single = SectionNbt {
            y: 0,
            block_palette: vec!["minecraft:air".to_owned()],
            block_data: Vec::new(),
            biome_palette: vec!["minecraft:plains".to_owned()],
            biome_data: Vec::new(),
            sky_light: None,
            block_light: None,
        };
        assert_eq!(single.block_bits(), 0);
        let mut two = single;
        two.block_palette.push("minecraft:grass_block".to_owned());
        assert_eq!(two.block_bits(), 4);
    }
}
