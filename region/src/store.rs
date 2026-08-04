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
    /// 被 `set_block` 修改的主世界区块（原流体 tick 全量扫描用；已改为仅含流体的区块）。
    dirty: std::collections::HashSet<(i32, i32)>,
    /// 流体相关位置变化（`set_block` 维护：放置/移除/邻居变化 → 计划 tick 调度，
    /// 对应 vanilla `LiquidBlock.onPlace`/`neighborChanged` → `scheduleTick`）。
    fluid_schedule: std::collections::HashSet<(i32, i32, i32)>,
    /// 含红石方块的主世界区块（红石 tick 用）。
    redstone_dirty: std::collections::HashSet<(i32, i32)>,
    /// 方块实体 NBT（世界坐标 → NBT；命令方块命令等 `/data` 操作目标）。
    block_entities: std::collections::HashMap<(i32, i32, i32), icbm_mc_767::nbt::Nbt>,
    /// 命令方块位置（`set_block` 维护；`tick_command_blocks` 遍历用，避免全量扫描）。
    command_blocks: std::collections::HashSet<(i32, i32, i32)>,
    /// 活塞位置（`set_block` 维护；`tick_pistons` 遍历用）。
    pistons: std::collections::HashSet<(i32, i32, i32)>,
    /// 作物位置（`set_block` 维护；`tick_crops` 遍历用，避免全量 heightmap 扫描）。
    crops: std::collections::HashSet<(i32, i32, i32)>,
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
            dirty: std::collections::HashSet::new(),
            fluid_schedule: std::collections::HashSet::new(),
            redstone_dirty: std::collections::HashSet::new(),
            block_entities: std::collections::HashMap::new(),
            command_blocks: std::collections::HashSet::new(),
            pistons: std::collections::HashSet::new(),
            crops: std::collections::HashSet::new(),
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
        if self.owns(face, cx, cz) {
            Ok(())
        } else {
            Err(StoreError::NotOwned(
                match face {
                    Face::Overworld => "overworld",
                    Face::Nether => "nether",
                },
                cx,
                cz,
            ))
        }
    }

    /// 区块是否属于本区域（主世界 32×32 / 地狱 4×4）。
    #[must_use]
    pub const fn owns(&self, face: Face, cx: i32, cz: i32) -> bool {
        match face {
            Face::Overworld => cx >> 5 == self.id.rx && cz >> 5 == self.id.rz,
            Face::Nether => cx >> 2 == self.id.rx && cz >> 2 == self.id.rz,
        }
    }

    /// 取区块（内存 → Anvil → flat 生成）。
    pub fn get_chunk(&mut self, face: Face, cx: i32, cz: i32) -> Result<&ChunkColumn, StoreError> {
        self.ensure_loaded(face, cx, cz)?;
        let map = match face {
            Face::Overworld => &self.overworld,
            Face::Nether => &self.nether,
        };
        Ok(map.get(&(cx, cz)).expect("chunk just inserted"))
    }

    /// 取区块（可变引用，供方块修改）。
    pub fn get_chunk_mut(
        &mut self,
        face: Face,
        cx: i32,
        cz: i32,
    ) -> Result<&mut ChunkColumn, StoreError> {
        self.ensure_loaded(face, cx, cz)?;
        let map = match face {
            Face::Overworld => &mut self.overworld,
            Face::Nether => &mut self.nether,
        };
        Ok(map.get_mut(&(cx, cz)).expect("chunk just inserted"))
    }

    /// 确保区块已加载（内存 → Anvil → flat 生成）。
    fn ensure_loaded(&mut self, face: Face, cx: i32, cz: i32) -> Result<(), StoreError> {
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
        Ok(())
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
        let column = match self.check_owned(face, cx, cz) {
            Ok(()) => self.get_chunk(face, cx, cz)?.clone(),
            Err(_) => match face {
                Face::Overworld => generate_chunk(GenDimension::Overworld, cx, cz),
                Face::Nether => generate_chunk(GenDimension::Nether, cx, cz),
            },
        };
        Ok(build_network_chunk(&column))
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

    /// 设置方块状态 ID（`setblock`/`fill`）。
    ///
    /// 返回 `(chunk_x, chunk_z, 新状态)`；区块不存在时先生成。越界或非本区域
    /// 区块返回 `Err(NotOwned)`（调用方按区域边界拒绝）。
    pub fn set_block(
        &mut self,
        face: Face,
        x: i32,
        y: i32,
        z: i32,
        state_id: i32,
    ) -> Result<(i32, i32), StoreError> {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.check_owned(face, cx, cz)?;
        let chunk = self.get_chunk_mut(face, cx, cz)?;
        let old_state = chunk.block_at(
            icbm_terrain_gen::ChunkColumn::local_coord(x),
            y,
            icbm_terrain_gen::ChunkColumn::local_coord(z),
        );
        let ok = chunk.set_block(
            icbm_terrain_gen::ChunkColumn::local_coord(x),
            y,
            icbm_terrain_gen::ChunkColumn::local_coord(z),
            state_id,
        );
        if !ok {
            return Err(StoreError::NotOwned("set_block out of range", cx, cz));
        }
        if face == Face::Overworld {
            self.dirty.insert((cx, cz));
            // 流体相关位置变化 → 计划 tick 调度（vanilla `LiquidBlock.onPlace`/
            // `neighborChanged` → `scheduleTick(pos, fluid, getTickDelay)`）。
            // 旧/新任一为流体都记录：放置时 onPlace 调度自身；移除/替换时
            // neighborChanged 会让相邻流体重新求值（方块事件循环内扩散目标
            // 也经 set_block 回流到这里）。
            if icbm_gameplay_minecraft::fluid::is_fluid(state_id)
                || icbm_gameplay_minecraft::fluid::is_fluid(old_state)
            {
                self.fluid_schedule.insert((x, y, z));
            }
            // 红石：旧/新状态任一为红石方块都重新计算——拆除电源/导线也必须
            // 触发相邻导线重新求值（旧实现只查新状态，拆掉红石块后导线永远
            // 保持点亮，红石“不传导/无法熄灭”）。同时标记正交邻接区块：
            // 电源在邻区时本区导线也要更新（跨区块传导）。
            if icbm_gameplay_minecraft::redstone::is_redstone_block(state_id)
                || icbm_gameplay_minecraft::redstone::is_redstone_block(old_state)
            {
                self.mark_redstone_region(cx, cz);
            }
            // 维护命令方块位置表（tick_command_blocks 增量遍历）
            if is_command_block(state_id) {
                self.command_blocks.insert((x, y, z));
            } else {
                self.command_blocks.remove(&(x, y, z));
            }
            // 维护活塞位置表（tick_pistons 增量遍历）
            if icbm_gameplay_minecraft::pistons::piston_info(state_id).is_some() {
                self.pistons.insert((x, y, z));
            } else {
                self.pistons.remove(&(x, y, z));
            }
        }
        Ok((cx, cz))
    }

    /// 取区块内方块状态 ID（供 region 模拟查询）。
    pub fn block_at(&mut self, face: Face, x: i32, y: i32, z: i32) -> Result<i32, StoreError> {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        self.check_owned(face, cx, cz)?;
        let column = self.get_chunk(face, cx, cz)?;
        Ok(column.block_at(
            icbm_terrain_gen::ChunkColumn::local_coord(x),
            y,
            icbm_terrain_gen::ChunkColumn::local_coord(z),
        ))
    }

    /// 已加载主世界区块坐标列表。
    #[must_use]
    pub fn loaded_overworld_chunks(&self) -> Vec<(i32, i32)> {
        self.overworld.keys().copied().collect()
    }

    /// 需要调度流体计划 tick 的位置（vanilla onPlace/neighborChanged），并清空标记。
    pub fn fluid_schedule_positions(&mut self) -> Vec<(i32, i32, i32)> {
        let positions: Vec<(i32, i32, i32)> = self.fluid_schedule.iter().copied().collect();
        self.fluid_schedule.clear();
        positions
    }

    /// 含红石方块的主世界区块（红石 tick 用），并清空标记。
    pub fn redstone_chunks(&mut self) -> Vec<(i32, i32)> {
        let chunks: Vec<(i32, i32)> = self.redstone_dirty.iter().copied().collect();
        self.redstone_dirty.clear();
        chunks
    }

    /// 标记区块及其正交邻接区块含红石（红石功率跨区块传播：电源在邻区时
    /// 本区导线也需重新计算；仅标记本区域拥有的区块）。
    pub fn mark_redstone_region(&mut self, cx: i32, cz: i32) {
        self.redstone_dirty.insert((cx, cz));
        for (nx, nz) in [(cx + 1, cz), (cx - 1, cz), (cx, cz + 1), (cx, cz - 1)] {
            if self.owns(Face::Overworld, nx, nz) {
                self.redstone_dirty.insert((nx, nz));
            }
        }
    }

    /// 读取方块实体 NBT（不存在返回 `None`）。
    #[must_use]
    pub fn block_entity(&self, x: i32, y: i32, z: i32) -> Option<&icbm_mc_767::nbt::Nbt> {
        self.block_entities.get(&(x, y, z))
    }

    /// 写入方块实体 NBT（`/data merge block` 与命令方块命令）。
    pub fn set_block_entity(&mut self, x: i32, y: i32, z: i32, nbt: icbm_mc_767::nbt::Nbt) {
        self.block_entities.insert((x, y, z), nbt);
    }

    /// 移除方块实体 NBT（`/data remove block`）。
    pub fn remove_block_entity(&mut self, x: i32, y: i32, z: i32) -> bool {
        self.block_entities.remove(&(x, y, z)).is_some()
    }

    /// 命令方块：读取 `Command` 字符串（NBT 缺省时取 `Command`/`command` 键）。
    pub fn command_block_command(&self, x: i32, y: i32, z: i32) -> Option<String> {
        use icbm_mc_767::nbt::Nbt;
        let nbt = self.block_entities.get(&(x, y, z))?;
        let Nbt::Compound(compound) = nbt else {
            return None;
        };
        compound
            .get_string("Command")
            .or_else(|| compound.get_string("command"))
            .map(str::to_owned)
    }

    /// 命令方块位置列表。
    #[must_use]
    pub fn command_block_positions(&self) -> Vec<(i32, i32, i32)> {
        self.command_blocks.iter().copied().collect()
    }

    /// 活塞位置列表。
    #[must_use]
    pub fn piston_positions(&self) -> Vec<(i32, i32, i32)> {
        self.pistons.iter().copied().collect()
    }

    /// 作物位置列表。
    #[must_use]
    pub fn crop_positions(&self) -> Vec<(i32, i32, i32)> {
        self.crops.iter().copied().collect()
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
    let min_y = bottom * 16;
    chunk.heightmaps = vec![
        (
            "WORLD_SURFACE".to_owned(),
            encode_heightmap(&heights, min_y).to_vec(),
        ),
        (
            "MOTION_BLOCKING".to_owned(),
            encode_heightmap(&heights, min_y).to_vec(),
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

/// 构建网络区块载荷。
fn build_network_chunk(column: &ChunkColumn) -> Vec<u8> {
    let heights = column.heightmap_heights();
    let mut sections = Vec::with_capacity(column.sections.len());
    for section in &column.sections {
        sections.push(ChunkSection::from_block_ids(section, column.biome_id));
    }
    let packet = CLevelChunkWithLight::flat(
        column.x,
        column.z,
        column.bottom_section_y * 16,
        heights,
        sections,
    );
    let mut writer = Writer::new();
    packet.encode(&mut writer);
    writer.into_bytes()
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

/// 从磁盘只读读取指定区域（已卸载）的某个区块并构建网络载荷。
///
/// 返回 `Ok(None)`：文件不存在 / 区块不存在 / 文件被活动 region 独占锁占用
/// （此时应改用进程内请求）。供 core 在玩家视野覆盖**已卸载** region 时直读
/// 其 .mca（对照 README「卸载的 region 可被 core 只读打开读取地图」）。
/// 活动（含沉睡）region 必须由进程内请求服务——其内存态可能比 .mca 新。
pub fn read_chunk_payload_from_disk(
    world_dir: &Path,
    rx: i32,
    rz: i32,
    cx: i32,
    cz: i32,
) -> Result<Option<Vec<u8>>, StoreError> {
    use icbm_anvil::region_file::{RegionFile, RegionFileError, chunk_index, local_coord};
    let path = world_dir.join("region").join(format!("r.{rx}.{rz}.mca"));
    let mut file = match RegionFile::open_readonly(&path) {
        Ok(file) => file,
        Err(RegionFileError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None); // 区域从未生成：由调用方 flat 兑底
        }
        Err(RegionFileError::Locked) => return Ok(None), // 区域仍存活：改用进程内请求
        Err(err) => return Err(err.into()),
    };
    let index = chunk_index(local_coord(cx), local_coord(cz));
    let Some(nbt) = file.read_chunk(index)? else {
        return Ok(None);
    };
    Ok(Some(build_network_chunk(&chunk_from_anvil(&nbt))))
}

/// 生成 flat 网络载荷（覆盖“区域从未生成/区块缺失”时的兑底；与 region 内存
/// 生成一致——确定性地形，固定种子可复现）。
#[must_use]
pub fn generate_chunk_payload(dim: u8, cx: i32, cz: i32) -> Vec<u8> {
    let column = generate_chunk(gen_dimension(dim), cx, cz);
    build_network_chunk(&column)
}

/// 是否命令方块状态（普通 7906..=7917 / 循环 12515..=12526 / 连锁 12527..=12538）。
#[must_use]
pub const fn is_command_block(state_id: i32) -> bool {
    (state_id >= 7906 && state_id <= 7917)
        || (state_id >= 12_515 && state_id <= 12_526)
        || (state_id >= 12_527 && state_id <= 12_538)
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
    fn store_serves_unowned_chunk_without_caching() {
        let dir = temp_world("reject");
        let id = RegionId { rx: 0, rz: 0 };
        let mut store = ChunkStore::open(&dir, id).unwrap();
        // 区域外区块可临时生成读取（不缓存、不计数）
        let payload = store
            .network_chunk_payload(Face::Overworld, 100, 100)
            .unwrap();
        assert!(payload.len() > 100);
        assert_eq!(store.loaded_count(), 0);
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
