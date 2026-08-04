//! playerdata/\<uuid\>.dat 读写。
//!
//! 玩家数据根复合（无 `Data` 包装）直接包含 `DataVersion`、`Pos`、
//! `Rotation`、`Dimension`、`abilities` 等字段。

use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use flate2::Compression as FlateCompression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::nbt::{Nbt, NbtCompound, NbtList, decode_named, encode_named};
use icbm_mc_767::protocol::DATA_VERSION;
use uuid::Uuid;

/// 玩家数据错误。
#[derive(Debug)]
pub enum PlayerDataError {
    /// IO 错误。
    Io(std::io::Error),
    /// NBT 错误。
    Nbt(icbm_mc_767::nbt::NbtError),
    /// 不支持的数据版本。
    BadDataVersion(i32),
}

impl fmt::Display for PlayerDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io: {err}"),
            Self::Nbt(err) => write!(f, "nbt: {err}"),
            Self::BadDataVersion(v) => write!(f, "unsupported DataVersion {v}"),
        }
    }
}

impl std::error::Error for PlayerDataError {}

impl From<std::io::Error> for PlayerDataError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<icbm_mc_767::nbt::NbtError> for PlayerDataError {
    fn from(value: icbm_mc_767::nbt::NbtError) -> Self {
        Self::Nbt(value)
    }
}

/// 读取玩家数据（不存在返回 `Ok(None)`）。
pub fn read_player(path: impl AsRef<Path>) -> Result<Option<Nbt>, PlayerDataError> {
    let compressed = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(PlayerDataError::Io(err)),
    };
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes)?;

    let mut reader = Reader::new(&bytes);
    let nbt = decode_named(&mut reader)?;
    if let Nbt::Compound(root) = &nbt
        && root.get_i32("DataVersion") != Some(DATA_VERSION)
    {
        return Err(PlayerDataError::BadDataVersion(
            root.get_i32("DataVersion").unwrap_or_default(),
        ));
    }
    Ok(Some(nbt))
}

/// 写出玩家数据（gzip 压缩的命名 NBT）。
pub fn write_player(path: impl AsRef<Path>, nbt: &Nbt) -> Result<(), PlayerDataError> {
    let mut writer = Writer::new();
    encode_named(nbt, &mut writer)?;

    let mut encoder = GzEncoder::new(Vec::new(), FlateCompression::default());
    encoder.write_all(&writer.into_bytes())?;
    let compressed = encoder.finish()?;

    let mut file = File::create(path)?;
    file.write_all(&compressed)?;
    Ok(())
}

/// playerdata 文件名。
#[must_use]
pub fn player_file_name(uuid: &Uuid) -> String {
    format!("{uuid}.dat")
}

/// 物品栏 NBT 编码（vanilla `Inventory.save`）：`Inventory` 列表，每项
/// `{Slot: byte, id: "minecraft:stone", count: n}`。槽号用**内部索引**：
/// 主栏 0..35（含热栏 27..35）、装备 100..103、副手 150（对照 `Inventory.java`）。
///
/// `item_name(core_slot)` 把网络槽号（`InventoryMenu`：主栏 9..44、装备 5..8、
/// 副手 45）映射为内部索引；空栈跳过。
#[must_use]
pub fn encode_inventory(items: &[Option<(String, i32)>]) -> Nbt {
    let mut list = NbtList::empty();
    for (i, item) in items.iter().enumerate() {
        let Some((name, count)) = item else {
            continue;
        };
        let Some(slot) = network_slot_to_internal(i) else {
            continue;
        };
        let mut entry = NbtCompound::new();
        entry.insert("Slot", Nbt::Byte(slot));
        entry.insert("id", Nbt::String(format!("minecraft:{name}")));
        entry.insert("count", Nbt::Byte(i8::try_from(*count).unwrap_or(1)));
        list.push(Nbt::Compound(Box::new(entry)));
    }
    Nbt::List(Box::new(list))
}

/// 解析 `Inventory` NBT 列表 → 46 槽 `(item, count)`（网络槽号序）。
/// 空/缺槽为 `None`。
#[must_use]
pub fn decode_inventory(list: &NbtList) -> [Option<(String, i32)>; 46] {
    let mut slots: [Option<(String, i32)>; 46] = std::array::from_fn(|_| None);
    for value in &list.values {
        let Nbt::Compound(entry) = value else {
            continue;
        };
        let Some(Nbt::Byte(slot)) = entry.get("Slot") else {
            continue;
        };
        let Some(Nbt::String(id)) = entry.get("id") else {
            continue;
        };
        let count = match entry.get("count") {
            Some(Nbt::Byte(n)) => i32::from(*n),
            Some(Nbt::Int(n)) => *n,
            _ => 1,
        };
        let Some(network_slot) = internal_slot_to_network(*slot) else {
            continue;
        };
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        slots[network_slot] = Some((name.to_owned(), count));
    }
    slots
}

/// 网络槽号（`InventoryMenu`：主栏 9..44、装备 5..8、副手 45）→ 内部索引
/// （主栏 0..35、装备 100..103、副手 150）。合成区 0..4 不持久化。
///
/// 返回 NBT `Slot` 值（vanilla `putByte`：i8；副手 150 存为 -106）。
#[must_use]
pub const fn network_slot_to_internal(slot: usize) -> Option<i8> {
    match slot {
        5..=8 => Some((100 + (slot - 5)) as i8), // 装备
        9..=44 => Some((slot - 9) as i8),        // 主栏（27..35 = 热栏）
        45 => Some(-106),                        // 副手（i8 的 -106 & 0xFF = 150）
        _ => None,
    }
}

/// NBT `Slot` 值（`& 255` 无符号）→ 网络槽号（[`network_slot_to_internal`] 逆映射）。
#[must_use]
pub const fn internal_slot_to_network(slot: i8) -> Option<usize> {
    let slot = slot as u8; // 150 → 0x96
    match slot {
        0..=35 => Some((slot as usize) + 9),
        100..=103 => Some((slot as usize) - 100 + 5),
        150 => Some(45),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{player_file_name, read_player, write_player};
    use icbm_mc_767::nbt::{Nbt, NbtCompound};
    use uuid::Uuid;

    #[test]
    fn player_data_round_trip() {
        let uuid = Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        let mut root = NbtCompound::new();
        root.insert("DataVersion", Nbt::Int(3955));
        root.insert("Dimension", Nbt::String("minecraft:overworld".to_owned()));
        let nbt = Nbt::Compound(Box::new(root));

        let path = std::env::temp_dir().join(player_file_name(&uuid));
        write_player(&path, &nbt).unwrap();
        let read = read_player(&path).unwrap().unwrap();
        assert!(matches!(read, Nbt::Compound(_)));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_player_is_none() {
        let path = std::env::temp_dir().join("nonexistent-player.dat");
        assert!(read_player(path).unwrap().is_none());
    }

    #[test]
    fn inventory_round_trip() {
        use super::{
            decode_inventory, encode_inventory, internal_slot_to_network, network_slot_to_internal,
        };
        use icbm_mc_767::nbt::Nbt;
        // 网络槽 36（热栏 0）、9（主栏 0）、5（装备）、45（副手）
        let mut items = [const { None }; 46];
        items[36] = Some(("obsidian".to_owned(), 64));
        items[9] = Some(("stone".to_owned(), 1));
        items[45] = Some(("shield".to_owned(), 1));
        let nbt = encode_inventory(&items);
        let Nbt::List(list) = &nbt else {
            panic!("expected list");
        };
        assert_eq!(list.values.len(), 3);
        let decoded = decode_inventory(list);
        assert_eq!(decoded[36], Some(("obsidian".to_owned(), 64)));
        assert_eq!(decoded[9], Some(("stone".to_owned(), 1)));
        assert_eq!(decoded[45], Some(("shield".to_owned(), 1)));
        assert!(decoded[0].is_none());
        // 槽号映射往返
        assert_eq!(network_slot_to_internal(36), Some(27));
        assert_eq!(internal_slot_to_network(27), Some(36));
        assert_eq!(network_slot_to_internal(5), Some(100));
        assert_eq!(internal_slot_to_network(100), Some(5));
        assert_eq!(network_slot_to_internal(45), Some(-106));
        assert_eq!(internal_slot_to_network(-106), Some(45));
        // 合成区/输出不持久化
        assert_eq!(network_slot_to_internal(0), None);
        assert_eq!(network_slot_to_internal(4), None);
    }
}
