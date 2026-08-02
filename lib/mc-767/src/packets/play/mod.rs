//! Play 阶段包与公共类型。

pub mod clientbound;
pub mod serverbound;

use crate::codec::Writer;

/// 世界坐标 BlockPos（网络编码为打包 i64：x/z 各 26 位、y 12 位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// 1.21.1 注册表网络 ID 常量（对照 master `data/src/generated`）。
///
/// 方块状态 ID 是固定全局 ID（客户端内置）；生物群系 ID 由服务器注册表
/// 数据决定（本实现采用 vanilla 字母序，见 [`crate::packets::play::biome_ids`]）。
pub mod block_state_ids {
    /// 空气。
    pub const AIR: i32 = 0;
    /// 石头。
    pub const STONE: i32 = 1;
    /// 草方块（snowy=false）。
    pub const GRASS_BLOCK: i32 = 8;
    /// 泥土。
    pub const DIRT: i32 = 10;
    /// 基岩。
    pub const BEDROCK: i32 = 80;
    /// 水（level 0）。
    pub const WATER: i32 = 81;
    /// 熔岩（level 0）。
    pub const LAVA: i32 = 97;
    /// 沙子。
    pub const SAND: i32 = 113;
    /// 黑曜石。
    pub const OBSIDIAN: i32 = 2355;
    /// 地狱岩。
    pub const NETHERRACK: i32 = 5850;
    /// 末地石。
    pub const END_STONE: i32 = 7416;
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
/// heightmap 长整型数量（256×9/64）。
pub const HEIGHTMAP_LONGS: usize = 36;

/// 编码 heightmap 长整型数组（LSB-first 打包，值为列高 = 最高方块 y + 1）。
#[must_use]
pub fn encode_heightmap(heights: &[u16; HEIGHTMAP_COLUMNS]) -> [i64; HEIGHTMAP_LONGS] {
    let mut longs = [0i64; HEIGHTMAP_LONGS];
    for (column, &height) in heights.iter().enumerate() {
        let bit_pos = column * HEIGHTMAP_BITS as usize;
        let long_index = bit_pos / 64;
        let offset = bit_pos % 64;
        longs[long_index] |= i64::from(height) << offset;
        let spill = offset + HEIGHTMAP_BITS as usize;
        if spill > 64 && long_index + 1 < HEIGHTMAP_LONGS {
            longs[long_index + 1] |= i64::from(height) >> (64 - offset);
        }
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
            writer.write_u8(0);
            writer.write_var_int(self.entries[0]);
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

/// 将索引数组 LSB-first 打包为 i64 数组。
#[must_use]
pub fn pack_indices(values: &[u32], bits: u32) -> Vec<i64> {
    let total_bits = values.len() * bits as usize;
    let long_count = total_bits.div_ceil(64);
    let mut longs = vec![0u64; long_count];
    for (index, &value) in values.iter().enumerate() {
        let bit_pos = index * bits as usize;
        let long_index = bit_pos / 64;
        let offset = bit_pos % 64;
        longs[long_index] |= u64::from(value) << offset;
        let spill = offset + bits as usize;
        if spill > 64 && long_index + 1 < long_count {
            longs[long_index + 1] |= u64::from(value) >> (64 - offset);
        }
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

/// 解包 LSB-first 打包数据（`count` 个值，每值 `bits` 位）。
#[must_use]
pub fn unpack_indices(data: &[i64], bits: u32, count: usize) -> Vec<u32> {
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let bit_pos = index * bits as usize;
        let long_index = bit_pos / 64;
        let offset = bit_pos % 64;
        let mut value = if long_index < data.len() {
            (data[long_index] as u64) >> offset
        } else {
            0
        };
        let spill = offset + bits as usize;
        if spill > 64 && long_index + 1 < data.len() {
            value |= (data[long_index + 1] as u64) << (64 - offset);
        }
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
    use super::{BlockPos, ceil_log2, encode_heightmap, encode_paletted_container};
    use crate::codec::{Reader, Writer};

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
        let heights = [4u16; 256];
        let longs = encode_heightmap(&heights);
        assert_eq!(longs.len(), 36);
        assert_eq!(longs[0], 0x100804020100804);
        assert_eq!((longs[35] >> 55) & 0x1FF, 4); // 列 255 位于位 55..64
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
        assert!(r.is_empty());
    }

    #[test]
    fn palette_array_two_entries() {
        // 第 0 个为 air，其余为 grass（4096 个，4 位打包 → 256 longs）
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
        assert_eq!(r.read_var_int().unwrap(), 256); // 4096*4/64
    }
}
