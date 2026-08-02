//! Play 阶段包与公共类型。

pub mod clientbound;
pub mod serverbound;

use crate::codec::Writer;

/// 世界坐标 BlockPos（网络编码为打包 i64：x/z 各 26 位、y 12 位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BlockPos {
    /// X。
    pub x: i32,
    /// Y。
    pub y: i32,
    /// Z。
    pub z: i32,
}

impl BlockPos {
    /// 新建坐标。
    #[must_use]
    pub const fn new(x: i32, y: i32, z: i32) -> Self {
        Self { x, y, z }
    }

    /// 编码为网络 i64。
    #[must_use]
    pub fn as_long(&self) -> i64 {
        (i64::from(self.x) & 0x3FF_FFFF) << 38
            | (i64::from(self.z) & 0x3FF_FFFF) << 12
            | (i64::from(self.y) & 0xFFF)
    }

    /// 从网络 i64 解码（x/z 各 26 位有符号、y 12 位有符号）。
    #[must_use]
    pub const fn from_long(value: i64) -> Self {
        Self {
            x: (value >> 38) as i32,
            z: ((value << 26) >> 38) as i32,
            y: ((value << 52) >> 52) as i32,
        }
    }
}

/// 1.21.1 注册表网络 ID 常量（对照 master `data/src/generated/block.rs`，即
/// vanilla `Block.BLOCK_STATE_REGISTRY` 的全局状态 ID）。
///
/// 方块状态 ID 是固定全局 ID（客户端内置）；生物群系 ID 由服务器注册表
/// 数据决定（本实现采用 vanilla 字母序，见 [`crate::packets::play::biome_ids`]）。
///
/// 注意：vanilla 按「方块注册序 + 状态属性值序」枚举状态；`BooleanProperty`
/// 的值序为 `[true, false]`（`BooleanProperty.java`：`ImmutableSet.of(true, false)`），
/// 因此 `grass_block` 两个状态中 **snowy=true 在前**（id 8）、snowy=false 在后（id 9）。
pub mod block_state_ids {
    /// 空气。
    pub const AIR: i32 = 0;
    /// 石头。
    pub const STONE: i32 = 1;
    /// 草方块（snowy=false，默认态）。id 8 是 snowy=true。
    pub const GRASS_BLOCK: i32 = 9;
    /// 泥土。
    pub const DIRT: i32 = 10;
    /// 基岩。
    pub const BEDROCK: i32 = 79;
    /// 水（level=0，默认态）。
    pub const WATER: i32 = 80;
    /// 熔岩（level=0，默认态）。
    pub const LAVA: i32 = 96;
    /// 沙子。
    pub const SAND: i32 = 112;
    /// 黑曜石。
    pub const OBSIDIAN: i32 = 2354;
    /// 地狱岩。
    pub const NETHERRACK: i32 = 5849;
    /// 末地石。
    pub const END_STONE: i32 = 7415;
    /// TNT（unstable=false，默认态）。
    pub const TNT: i32 = 2095;
    /// 橡木原木（axis=y，默认态）。
    pub const OAK_LOG: i32 = 131;
    /// 橡木木板。
    pub const OAK_PLANKS: i32 = 15;
    /// 圆石。
    pub const COBBLESTONE: i32 = 14;
    /// 下界传送门方块（axis=x，默认态）。
    pub const NETHER_PORTAL: i32 = 5864;
    /// 红石线（power=0，默认态；power 0..15 → 4138..4153）。
    pub const REDSTONE_WIRE: i32 = 4138;
    /// 红石火把（熄灭态）。
    pub const REDSTONE_TORCH_OFF: i32 = 5738;
    /// 红石火把（点亮态）。
    pub const REDSTONE_TORCH_ON: i32 = 5739;
    /// 红石灯（熄灭态）。
    pub const REDSTONE_LAMP: i32 = 7418;
    /// 红石灯（点亮态）。
    pub const REDSTONE_LAMP_ON: i32 = 7419;
    /// 红石块（强电源）。
    pub const REDSTONE_BLOCK: i32 = 9223;
}

/// 1.21.1 生物群系网络 ID（icbm 服务器自定义注册表顺序）。
///
/// 注意：与 vanilla 字母序不同。客户端注册表由配置阶段的
/// `registry_data` 按本顺序建立，chunk biome palette 使用这些 ID。
pub mod biome_ids {
    /// 虚空。
    pub const THE_VOID: i32 = 0;
    /// 平原。
    pub const PLAINS: i32 = 1;
    /// 地狱荒原。
    pub const NETHER_WASTES: i32 = 2;
    /// 末地。
    pub const THE_END: i32 = 3;
}

/// 维度类型注册表网络 ID（顺序即注册表数据顺序，见 core 配置阶段）。
pub mod dimension_type_ids {
    /// 主世界。
    pub const OVERWORLD: i32 = 0;
    /// 主世界洞穴。
    pub const OVERWORLD_CAVES: i32 = 1;
    /// 末地。
    pub const THE_END: i32 = 2;
    /// 地狱。
    pub const THE_NETHER: i32 = 3;
}

/// 世界维度名。
pub mod dimension_names {
    /// 主世界。
    pub const OVERWORLD: &str = "minecraft:overworld";
    /// 地狱。
    pub const THE_NETHER: &str = "minecraft:the_nether";
    /// 末地。
    pub const THE_END: &str = "minecraft:the_end";
}

/// 区块 heightmap 每列位数。
pub const HEIGHTMAP_BITS: u32 = 9;
/// 区块列数（16×16）。
pub const HEIGHTMAP_COLUMNS: usize = 256;
/// 每长整型列数（`64 / 9`，整数除法，对照 `PackedIntegerArray.java` 的
/// `elementsPerLong = 64 / elementBits`）。
pub const HEIGHTMAP_COLUMNS_PER_LONG: usize = 7;
/// heightmap 长整型数量（`ceil(256 / 7) = 37`，对照 `PackedIntegerArray`
/// 构造的 `(size + elementsPerLong - 1) / elementsPerLong`；非旧的 `256×9/64`）。
pub const HEIGHTMAP_LONGS: usize = 37;

/// 编码 heightmap 长整型数组（1.21.1 `PackedIntegerArray` 布局：第 `c` 列位于
/// 第 `c/7` 个长整型的第 `(c%7)*9` 位，LSB-first，不跨长整型边界）。
///
/// 存储值 = 列高 − `min_y`，即 vanilla `Heightmap.set` 的
/// `height - chunk.getBottomY()`（`height = 最高方块 y + 1`）；`heights` 传入
/// 绝对列高（最高非空气方块 y + 1）。
#[must_use]
pub fn encode_heightmap(heights: &[u16; HEIGHTMAP_COLUMNS], min_y: i32) -> [i64; HEIGHTMAP_LONGS] {
    let mut longs = [0i64; HEIGHTMAP_LONGS];
    for (column, &height) in heights.iter().enumerate() {
        let stored = i32::from(height) - min_y;
        let long_index = column / HEIGHTMAP_COLUMNS_PER_LONG;
        let offset = (column % HEIGHTMAP_COLUMNS_PER_LONG) * HEIGHTMAP_BITS as usize;
        longs[long_index] |= i64::from(stored) << offset;
    }
    longs
}

/// 编码 `PalettedContainer` 网络载荷（1.21.1 规则）。
///
/// - bits=0：单值 palette（直接写 1 个 VarInt，无数量前缀）
/// - bits ≤ `array_max_bits`：间接 palette（写数量 + 条目），存储位数见
///   `storage_bits`（方块 1-4 位强制 4 位）
/// - 其余：直接模式（写 `direct_bits`，无 palette）
pub fn encode_paletted_container(
    writer: &mut Writer,
    values: &[i32],
    array_max_bits: u32,
    storage_bits: impl Fn(u32) -> u32,
    direct_bits: u32,
) {
    Palette::from_ids(values, array_max_bits, &storage_bits, direct_bits).encode(writer);
}

/// 解析后的 palette（条目 + 打包数据），网络与存档共用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// 存储位数（0 = 单值）。
    pub bits: u32,
    /// palette 条目（直接模式为空）。
    pub entries: Vec<i32>,
    /// 打包数据（LSB-first；单值模式为空）。
    pub data: Vec<i64>,
}

impl Palette {
    /// 从原始值构造（按 1.21.1 `PaletteProvider` 规则）。
    #[must_use]
    pub fn from_ids(
        ids: &[i32],
        array_max_bits: u32,
        storage_bits: &impl Fn(u32) -> u32,
        direct_bits: u32,
    ) -> Self {
        let mut entries: Vec<i32> = Vec::new();
        let mut indices: Vec<u32> = Vec::with_capacity(ids.len());
        for &value in ids {
            if let Some(idx) = entries.iter().position(|&v| v == value) {
                indices.push(idx as u32);
            } else {
                entries.push(value);
                indices.push(entries.len() as u32 - 1);
            }
        }

        let bits = ceil_log2(entries.len() as u32);
        if bits == 0 {
            return Self {
                bits: 0,
                entries,
                data: Vec::new(),
            };
        }
        if bits <= array_max_bits {
            let storage = storage_bits(bits);
            Self {
                bits: storage,
                entries,
                data: pack_indices(&indices, storage),
            }
        } else {
            Self {
                bits: direct_bits,
                entries: Vec::new(),
                data: pack_indices(
                    &ids.iter().map(|&v| v as u32).collect::<Vec<_>>(),
                    direct_bits,
                ),
            }
        }
    }

    /// 方块 palette（1.21.1：0 单值 / 1-4 → 4 位 / 5-8 → bimap / >8 → 15 位）。
    #[must_use]
    pub fn for_blocks(ids: &[i32]) -> Self {
        Self::from_ids(ids, 8, &|bits| bits.max(4), 15)
    }

    /// 生物群系 palette（1.21.1：0 单值 / 1-3 → 数组 / >3 → 6 位）。
    #[must_use]
    pub fn for_biomes(ids: &[i32]) -> Self {
        Self::from_ids(ids, 3, &|bits| bits, 6)
    }

    /// 写出网络载荷。
    pub fn encode(&self, writer: &mut Writer) {
        if self.bits == 0 {
            // 单值模式：bits=0 + palette 值 + data 长度 0（官方 ZeroBitStorage
            // 的 getRaw 返回固定 31-long 数组，writeLongArray 仍写 varint 0）
            writer.write_u8(0);
            writer.write_var_int(self.entries[0]);
            writer.write_var_int(0);
            return;
        }
        writer.write_u8(self.bits as u8);
        if !self.entries.is_empty() {
            writer.write_var_int(i32::try_from(self.entries.len()).expect("palette exceeds i32"));
            for id in &self.entries {
                writer.write_var_int(*id);
            }
        }
        write_packed_indices(writer, &self.data);
    }
}

/// 将索引数组按 1.21.1 `PackedIntegerArray` 布局打包：每长整型容纳
/// `64 / bits` 个元素（整数除法），元素不跨长整型边界；第 `i` 个元素位于
/// 第 `i / per_long` 个长整型的第 `(i % per_long) * bits` 位（LSB-first）。
///
/// 与旧版 `BitStorage` 的“跨长整型 spill”布局不兼容（1.21 起 Mojang 以
/// `PackedIntegerArray.java` 取代 `BitStorage`，网络与存档共用此布局）。
#[must_use]
pub fn pack_indices(values: &[u32], bits: u32) -> Vec<i64> {
    assert!(bits > 0 && bits <= 64, "pack bits out of range: {bits}");
    let per_long = (64 / bits) as usize;
    let long_count = values.len().div_ceil(per_long);
    let mut longs = vec![0u64; long_count];
    for (index, &value) in values.iter().enumerate() {
        let long_index = index / per_long;
        let offset = (index % per_long) * bits as usize;
        longs[long_index] |= u64::from(value) << offset;
    }
    longs.iter().map(|&v| v as i64).collect()
}

/// 将打包后的 i64 数组写出（VarInt 长度 + 大端 i64）。
fn write_packed_indices(writer: &mut Writer, data: &[i64]) {
    writer.write_var_int(i32::try_from(data.len()).expect("long array exceeds i32"));
    for value in data {
        writer.write_i64_be(*value);
    }
}

/// 解包 `PackedIntegerArray` 布局数据（`count` 个值，每值 `bits` 位；与
/// [`pack_indices`] 互逆）。
#[must_use]
pub fn unpack_indices(data: &[i64], bits: u32, count: usize) -> Vec<u32> {
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let per_long = (64 / bits) as usize;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let long_index = index / per_long;
        let offset = (index % per_long) * bits as usize;
        let value = if long_index < data.len() {
            (data[long_index] as u64) >> offset
        } else {
            0
        };
        values.push((value & mask) as u32);
    }
    values
}

/// 计算以 2 为底向上取整的对数（0 → 0）。
#[must_use]
pub const fn ceil_log2(value: u32) -> u32 {
    if value <= 1 {
        return 0;
    }
    32 - (value - 1).leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::{
        BlockPos, HEIGHTMAP_COLUMNS_PER_LONG, block_state_ids, ceil_log2, encode_heightmap,
        encode_paletted_container, unpack_indices,
    };
    use crate::codec::{Reader, Writer};

    /// golden：全局方块状态 ID 对照 vanilla 1.21.1 `Block.BLOCK_STATE_REGISTRY`
    /// （以 master `data/src/generated/block.rs` 为准）。
    ///
    /// 注意 `BooleanProperty` 值序为 `[true, false]`（`BooleanProperty.java`：
    /// `ImmutableSet.of(true, false)`），故 `grass_block[snowy=true]`=8 在前、
    /// `snowy=false`=9 在后；bedrock=79 之后是 water（80..）。
    #[test]
    fn block_state_ids_match_vanilla_1_21_1() {
        assert_eq!(block_state_ids::AIR, 0);
        assert_eq!(block_state_ids::STONE, 1);
        assert_eq!(block_state_ids::GRASS_BLOCK, 9); // snowy=false
        assert_eq!(block_state_ids::DIRT, 10);
        assert_eq!(block_state_ids::BEDROCK, 79);
        assert_eq!(block_state_ids::WATER, 80); // level=0
        assert_eq!(block_state_ids::LAVA, 96); // level=0
        assert_eq!(block_state_ids::SAND, 112);
        assert_eq!(block_state_ids::OBSIDIAN, 2354);
        assert_eq!(block_state_ids::NETHERRACK, 5849);
        assert_eq!(block_state_ids::END_STONE, 7415);
    }
    #[test]
    fn block_pos_round_trip() {
        for pos in [
            BlockPos::new(0, 0, 0),
            BlockPos::new(1, -60, 2),
            BlockPos::new(-100, 300, 50),
            BlockPos::new(0x1FF_FFFF, 0x7FF, -0x1FF_FFFF),
            BlockPos::new(-0x200_0000, -0x800, 0x1FF_FFFF),
        ] {
            assert_eq!(BlockPos::from_long(pos.as_long()), pos);
        }
    }

    #[test]
    fn ceil_log2_values() {
        assert_eq!(ceil_log2(0), 0);
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(256), 8);
    }

    #[test]
    fn heightmap_encoding() {
        // overworld min_y=-64，列高 4 → 存储值 4-(-64)=68
        let heights = [4u16; 256];
        let longs = encode_heightmap(&heights, -64);
        assert_eq!(longs.len(), 37); // 256/7 → 37（非 36）
        // 列 0..6 位于长整型 0；每长整型 7 列
        for column in 0..HEIGHTMAP_COLUMNS_PER_LONG {
            let offset = column * 9;
            assert_eq!((longs[0] >> offset) & 0x1FF, 68, "column {column}");
        }
        // 列 7 起在长整型 1：列 7 → 长整型 1 位 0
        assert_eq!(longs[1] & 0x1FF, 68);
        // 列 252..255 → 长整型 36（末字），位 0/9/18/27（68 = 0x44）
        assert_eq!(
            longs[36],
            (0x44i64) | (0x44i64 << 9) | (0x44i64 << 18) | (0x44i64 << 27)
        );
        // 单列非零：列 7 值 1（min_y=0 → 存储 1）只落在长整型 1 位 0
        let mut sparse = [0u16; 256];
        sparse[7] = 1;
        let longs = encode_heightmap(&sparse, 0);
        assert_eq!(longs[0], 0);
        assert_eq!(longs[1] & 0x1FF, 1);
        assert_eq!(longs[1] & !0x1FF, 0);
    }

    #[test]
    fn heightmap_round_trip() {
        let mut heights = [0u16; 256];
        for (i, h) in heights.iter_mut().enumerate() {
            *h = u16::try_from((i * 37) % 385 + 1).unwrap();
        }
        let longs = encode_heightmap(&heights, -64);
        let unpacked = unpack_indices(&longs, 9, 256);
        for (column, &h) in heights.iter().enumerate() {
            assert_eq!(unpacked[column], u32::from(h) + 64, "column {column}");
        }
    }

    #[test]
    fn palette_single() {
        let values = vec![1i32; 4096];
        let mut w = Writer::new();
        encode_paletted_container(&mut w, &values, 8, |b| b.max(4), 15);
        let bytes = w.into_bytes();
        assert_eq!(bytes[0], 0); // bits=0
        let mut r = Reader::new(&bytes[1..]);
        assert_eq!(r.read_var_int().unwrap(), 1);
        // 官方单值（ZeroBitStorage）仍写 data 长度 0
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert!(r.is_empty());
    }

    #[test]
    fn palette_array_two_entries() {
        // 第 0 个为 air，其余为 grass（4096 个，4 位打包 → 每长整型 16 个 → 256 longs）
        let mut values = vec![8i32; 4096];
        values[0] = 0;
        let mut w = Writer::new();
        encode_paletted_container(&mut w, &values, 8, |b| b.max(4), 15);
        let bytes = w.into_bytes();
        assert_eq!(bytes[0], 4); // 4 位数组
        let mut r = Reader::new(&bytes[1..]);
        assert_eq!(r.read_var_int().unwrap(), 2); // palette 数量
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.read_var_int().unwrap(), 8);
        let data_len = r.read_var_int().unwrap();
        assert_eq!(data_len, 256); // 4096/16
        let data: Vec<i64> = (0..data_len).map(|_| r.read_i64_be().unwrap()).collect();
        assert!(r.is_empty());
        // 解包回索引：第 0 个为 0，其余为 1（grass 的 palette 索引）
        let indices = unpack_indices(&data, 4, 4096);
        assert_eq!(indices[0], 0);
        assert!(indices[1..].iter().all(|&i| i == 1));
        // 长整型 0：索引 0=0 在低 4 位，其余 15 个索引 1
        assert_eq!(data[0] & 0xFFFF_FFFF, 0x1111_1110);
        assert_eq!(data[255], 0x1111_1111_1111_1111); // 末字全 1（4 位 16 个）
    }

    #[test]
    fn palette_packed_layout_matches_vanilla() {
        // 直接模式 15 位：每长整型 4 个（64/15），1024 个长整型（非 spill 的 960）
        let values: Vec<u32> = (0..4096).map(|i| u32::try_from(i % 300).unwrap()).collect();
        let packed = super::pack_indices(&values, 15);
        assert_eq!(packed.len(), 1024);
        let unpacked = unpack_indices(&packed, 15, 4096);
        assert_eq!(unpacked, values);
        // 生物群系 6 位：每长整型 10 个，64 元素 → 7 个长整型
        let biomes: Vec<u32> = (0..64).map(|i| i % 5).collect();
        let packed = super::pack_indices(&biomes, 6);
        assert_eq!(packed.len(), 7);
        let unpacked = unpack_indices(&packed, 6, 64);
        assert_eq!(unpacked, biomes);
    }
}
