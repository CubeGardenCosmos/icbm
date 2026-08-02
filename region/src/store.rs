//! 区块存储：内存列 + 主世界 Anvil 持久化 + 网络序列化。
//!
//! 区域模型：主世界面 = 一个 .mca（独占 flock，权威租约）；地狱面 Phase 1
//! 仅内存（flat 确定性可重建，跨区域共享 .mca 的协作持久化留待后续）。

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use icbm_anvil::chunk::{ChunkNbt, SectionNbt};
use icbm_anvil::region_file::{RegionFile, RegionFileError, chunk_index};
use icbm_mc_767::codec::Writer;
use icbm_mc_767::packets::play::clientbound::{CLevelChunkWithLight, ChunkSection};
use icbm_mc_767::packets::play::{Palette, encode_heightmap, unpack_indices};
use icbm_mc_767::registry::{name_to_state_id, state_id_to_name};
use icbm_terrain_gen::{ChunkColumn, Dimension as GenDimension, generate_chunk};
use tracing::debug;

use crate::protocol::DIM_NETHER;
use crate::{Face, RegionId};

/// 存储错误。
#[derive(Debug)]
pub enum StoreError {
    /// IO 错误。
    Io(std::io::Error),
    /// Anvil 错误。
    Anvil(RegionFileError),
    /// 区域不属于本进程。
    NotOwned(&'static str, i32, i32),
    /// 区块解析不一致。
    Inconsistent(&'static str),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io: {err}"),
            Self::Anvil(err) => write!(f, "anvil: {err}"),
            Self::NotOwned(face, cx, cz) => {
                write!(f, "chunk {face} ({cx},{cz}) not owned by this region")
            }
            Self::Inconsistent(msg) => write!(f, "inconsistent chunk data: {msg}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<RegionFileError> for StoreError {
    fn from(value: RegionFileError) -> Self {
        Self::Anvil(value)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// 区块存储。
pub struct ChunkStore {
    id: RegionId,
    overworld_file: RegionFile,
    overworld: HashMap<(i32, i32), ChunkColumn>,
    nether: HashMap<(i32, i32), ChunkColumn>,
    loaded: u32,
}

impl ChunkStore {
    /// 打开（创建主世界 .mca 并加独占锁）。
    pub fn open(world_dir: impl AsRef<Path>, id: RegionId) -> Result<Self, StoreError> {
        let region_dir = world_dir.as_ref().join("region");
        std::fs::create_dir_all(&region_dir)?;
        let path = region_dir.join(format!("r.{}.{}.mca", id.rx, id.rz));
        let overworld_file = RegionFile::open(path)?;
        Ok(Self {
            id,
            overworld_file,
            overworld: HashMap::new(),
            nether: HashMap::new(),
            loaded: 0,
        })
    }

    /// 区域 ID。
    #[must_use]
    pub const fn id(&self) -> &RegionId {
        &self.id
    }

    /// 已加载区块数。
    #[must_use]
    pub const fn loaded_count(&self) -> u32 {
        self.loaded
    }

    /// 校验区块归属。
    const fn check_owned(&self, face: Face, cx: i32, cz: i32) -> Result<(), StoreError> {
        match face {
            Face::Overworld => {
                if cx >> 5 == self.id.rx && cz >> 5 == self.id.rz {
                    Ok(())
                } else {
                    Err(StoreError::NotOwned("overworld", cx, cz))
                }
            }
            Face::Nether => {
                if cx >> 2 == self.id.rx && cz >> 2 == self.id.rz {
                    Ok(())
                } else {
                    Err(StoreError::NotOwned("nether", cx, cz))
                }
            }
        }
    }

    /// 取区块（内存 → Anvil → flat 生成）。
    pub fn get_chunk(&mut self, face: Face, cx: i32, cz: i32) -> Result<&ChunkColumn, StoreError> {
        self.check_owned(face, cx, cz)?;
        let loaded = match face {
            Face::Overworld => self.overworld.contains_key(&(cx, cz)),
            Face::Nether => self.nether.contains_key(&(cx, cz)),
        };
        if !loaded {
            let column = match face {
                Face::Overworld => self.load_overworld_chunk(cx, cz)?,
                Face::Nether => generate_chunk(GenDimension::Nether, cx, cz),
            };
            debug!(cx, cz, "loaded chunk");
            match face {
                Face::Overworld => self.overworld.insert((cx, cz), column),
                Face::Nether => self.nether.insert((cx, cz), column),
            };
            self.loaded += 1;
        }
        let map = match face {
            Face::Overworld => &self.overworld,
            Face::Nether => &self.nether,
        };
        Ok(map.get(&(cx, cz)).expect("chunk just inserted"))
    }

    /// 从 Anvil 加载或生成主世界区块。
    fn load_overworld_chunk(&mut self, cx: i32, cz: i32) -> Result<ChunkColumn, StoreError> {
        let index = chunk_index(
            icbm_anvil::region_file::local_coord(cx),
            icbm_anvil::region_file::local_coord(cz),
        );
        self.overworld_file.read_chunk(index)?.map_or_else(
            || Ok(generate_chunk(GenDimension::Overworld, cx, cz)),
            |nbt| Ok(chunk_from_anvil(&nbt)),
        )
    }

    /// 网络区块载荷（`LEVEL_CHUNK_WITH_LIGHT` 的数据部分，不含包 ID）。
    pub fn network_chunk_payload(
        &mut self,
        face: Face,
        cx: i32,
        cz: i32,
    ) -> Result<Vec<u8>, StoreError> {
        let column = self.get_chunk(face, cx, cz)?;
        let heights = column.heightmap_heights();
        let mut sections = Vec::with_capacity(column.sections.len());
        for section in &column.sections {
            sections.push(ChunkSection::from_block_ids(section, column.biome_id));
        }
        let packet = CLevelChunkWithLight::flat(cx, cz, heights, sections);
        let mut writer = Writer::new();
        packet.encode(&mut writer);
        Ok(writer.into_bytes())
    }

    /// 落盘主世界 .mca（卸载前调用）。
    pub fn flush(&mut self) -> Result<(), StoreError> {
        for (&(cx, cz), column) in &self.overworld {
            let index = chunk_index(
                icbm_anvil::region_file::local_coord(cx),
                icbm_anvil::region_file::local_coord(cz),
            );
            self.overworld_file
                .write_chunk(index, &chunk_to_anvil(column))?;
        }
        self.overworld_file.flush()?;
        Ok(())
    }
}

/// `ChunkColumn` → Anvil `SectionNbt` 列表。
fn chunk_to_anvil(column: &ChunkColumn) -> ChunkNbt {
    let bottom = column.bottom_section_y;
    let mut sections = Vec::with_capacity(column.sections.len());
    for (offset, blocks) in column.sections.iter().enumerate() {
        let section_y = bottom + offset as i32;
        let palette = Palette::for_blocks(blocks);
        let block_palette: Vec<String> = palette
            .entries
            .iter()
            .map(|&id| {
                state_id_to_name(id)
                    .expect("block id must be registered")
                    .to_owned()
            })
            .collect();
        let biome_name = biome_name(column.biome_id).to_owned();
        sections.push(SectionNbt {
            y: section_y,
            block_palette,
            block_data: palette.data,
            biome_palette: vec![biome_name],
            biome_data: Vec::new(),
            sky_light: (section_y >= 0).then(|| vec![0xFFu8; 2048]),
            block_light: None,
        });
    }
    let mut chunk = ChunkNbt::new(column.x, column.z, bottom, sections);
    let heights = column.heightmap_heights();
    chunk.heightmaps = vec![
        (
            "WORLD_SURFACE".to_owned(),
            encode_heightmap(&heights).to_vec(),
        ),
        (
            "MOTION_BLOCKING".to_owned(),
            encode_heightmap(&heights).to_vec(),
        ),
    ];
    chunk
}

/// Anvil `ChunkNbt` → `ChunkColumn`。
fn chunk_from_anvil(nbt: &ChunkNbt) -> ChunkColumn {
    let mut sections = Vec::with_capacity(nbt.sections.len());
    for section in &nbt.sections {
        let mut blocks = [0i32; 4096];
        let ids: Vec<i32> = section
            .block_palette
            .iter()
            .map(|name| name_to_state_id(name).unwrap_or(0))
            .collect();
        if let Some(first) = section.block_data.is_empty().then(|| ids.first()).flatten() {
            blocks.fill(*first);
        } else if !section.block_data.is_empty() {
            let indices = unpack_indices(&section.block_data, section.block_bits(), 4096);
            for (index, &palette_index) in indices.iter().enumerate() {
                let id = ids.get(palette_index as usize).copied().unwrap_or(0);
                blocks[index] = id;
            }
        }
        sections.push(blocks);
    }
    let biome_id = nbt
        .sections
        .first()
        .and_then(|s| s.biome_palette.first())
        .map_or(1, |name| biome_id(name));
    ChunkColumn {
        x: nbt.x,
        z: nbt.z,
        bottom_section_y: nbt.bottom_section_y,
        sections,
        biome_id,
        surface_height: 0,
    }
}

/// 生物群系网络 ID → 名（icbm 注册表顺序）。
#[must_use]
pub const fn biome_name(id: i32) -> &'static str {
    match id {
        2 => "minecraft:nether_wastes",
        3 => "minecraft:the_end",
        0 => "minecraft:the_void",
        _ => "minecraft:plains",
    }
}

/// 生物群系名 → 网络 ID。
#[must_use]
pub fn biome_id(name: &str) -> i32 {
    match name {
        "minecraft:nether_wastes" => 2,
        "minecraft:the_end" => 3,
        "minecraft:the_void" => 0,
        _ => 1,
    }
}

/// 维度字节 → 生成维度。
#[must_use]
pub const fn gen_dimension(dim_byte: u8) -> GenDimension {
    if dim_byte == DIM_NETHER {
        GenDimension::Nether
    } else {
        GenDimension::Overworld
    }
}

#[cfg(test)]
mod tests {
    use super::{ChunkStore, chunk_from_anvil, chunk_to_anvil};
    use crate::{Face, RegionId};
    use icbm_terrain_gen::Dimension as GenDimension;
    use icbm_terrain_gen::generate_chunk;

    fn temp_world(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("region-store-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn store_round_trip_overworld() {
        let dir = temp_world("round-trip");
        let id = RegionId { rx: 0, rz: 0 };
        let mut store = ChunkStore::open(&dir, id).unwrap();
        let payload = store.network_chunk_payload(Face::Overworld, 3, 5).unwrap();
        assert!(payload.len() > 100);
        assert_eq!(store.loaded_count(), 1);
        store.flush().unwrap();
        drop(store);

        // 重新打开：chunk 应从 Anvil 加载
        let mut reopened = ChunkStore::open(&dir, id).unwrap();
        let _ = reopened
            .network_chunk_payload(Face::Overworld, 3, 5)
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn store_rejects_unowned_chunk() {
        let dir = temp_world("reject");
        let id = RegionId { rx: 0, rz: 0 };
        let mut store = ChunkStore::open(&dir, id).unwrap();
        assert!(
            store
                .network_chunk_payload(Face::Overworld, 100, 100)
                .is_err()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn anvil_conversion_round_trip() {
        let column = generate_chunk(GenDimension::Overworld, 7, -3);
        let nbt = chunk_to_anvil(&column);
        let back = chunk_from_anvil(&nbt);
        assert_eq!(back.sections, column.sections);
        assert_eq!(back.biome_id, column.biome_id);
        assert_eq!(back.bottom_section_y, column.bottom_section_y);
    }
}
