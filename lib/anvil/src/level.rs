//! level.dat 读写（世界根数据）。
//!
//! 根复合为 `Data`，含 `DataVersion`（3955）、`LevelName`、`GameRules`、
//! `Player`、`WorldGenSettings` 等。Phase 1 只实现最小可写集与稳健解析。

use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use flate2::Compression as FlateCompression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::nbt::{Nbt, NbtCompound, NbtList, TagId, decode_named, encode_named};
use icbm_mc_767::protocol::DATA_VERSION;

/// level.dat 错误。
#[derive(Debug)]
pub enum LevelError {
    /// IO 错误。
    Io(std::io::Error),
    /// NBT 错误。
    Nbt(icbm_mc_767::nbt::NbtError),
    /// 缺少 Data 根复合。
    MissingData,
    /// 不支持的数据版本。
    BadDataVersion(i32),
}

impl fmt::Display for LevelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io: {err}"),
            Self::Nbt(err) => write!(f, "nbt: {err}"),
            Self::MissingData => f.write_str("level.dat missing Data root"),
            Self::BadDataVersion(v) => write!(f, "unsupported DataVersion {v}"),
        }
    }
}

impl std::error::Error for LevelError {}

impl From<std::io::Error> for LevelError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<icbm_mc_767::nbt::NbtError> for LevelError {
    fn from(value: icbm_mc_767::nbt::NbtError) -> Self {
        Self::Nbt(value)
    }
}

/// 世界根数据（最小集）。
#[derive(Debug, Clone, Default)]
pub struct LevelData {
    /// 世界名。
    pub level_name: String,
    /// 游戏规则（名 → 值）。
    pub gamerules: Vec<(String, String)>,
    /// 玩家根数据（原始 NBT，落盘到 level.dat 的 `Player` 与 playerdata 共用）。
    pub player: Option<Nbt>,
}

/// 读取 level.dat（gzip 压缩的命名 NBT）。
pub fn read_level_dat(path: impl AsRef<Path>) -> Result<LevelData, LevelError> {
    let compressed = std::fs::read(path)?;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes)?;

    let mut reader = Reader::new(&bytes);
    let nbt = decode_named(&mut reader)?;
    let Nbt::Compound(root) = nbt else {
        return Err(LevelError::MissingData);
    };
    let data = root.get_compound("Data").ok_or(LevelError::MissingData)?;

    if data.get_i32("DataVersion") != Some(DATA_VERSION) {
        return Err(LevelError::BadDataVersion(
            data.get_i32("DataVersion").unwrap_or_default(),
        ));
    }

    let mut gamerules = Vec::new();
    if let Some(rules) = data.get_compound("GameRules") {
        for (name, value) in &rules.entries {
            if let Nbt::String(v) = value {
                gamerules.push((name.clone(), v.clone()));
            }
        }
    }

    Ok(LevelData {
        level_name: data.get_string("LevelName").unwrap_or("world").to_owned(),
        gamerules,
        player: data.get("Player").cloned(),
    })
}

/// 写出 level.dat（gzip 压缩的命名 NBT）。
pub fn write_level_dat(path: impl AsRef<Path>, data: &LevelData) -> Result<(), LevelError> {
    let mut root = NbtCompound::new();
    let mut data_compound = NbtCompound::new();
    data_compound.insert("DataVersion", Nbt::Int(DATA_VERSION));

    let mut version = NbtCompound::new();
    version.insert("Id", Nbt::Int(DATA_VERSION));
    version.insert("Name", Nbt::String("1.21.1".to_owned()));
    version.insert("Snapshot", Nbt::Byte(0));
    data_compound.insert("Version", Nbt::Compound(Box::new(version)));

    data_compound.insert("LevelName", Nbt::String(data.level_name.clone()));

    let mut rules = NbtCompound::new();
    for (name, value) in &data.gamerules {
        rules.insert(name.clone(), Nbt::String(value.clone()));
    }
    data_compound.insert("GameRules", Nbt::Compound(Box::new(rules)));

    if let Some(player) = &data.player {
        data_compound.insert("Player", player.clone());
    }

    let mut worldgen = NbtCompound::new();
    worldgen.insert("bonus_chest", Nbt::Byte(0));
    data_compound.insert("WorldGenSettings", Nbt::Compound(Box::new(worldgen)));

    root.insert("Data", Nbt::Compound(Box::new(data_compound)));

    let mut writer = Writer::new();
    encode_named(&Nbt::Compound(Box::new(root)), &mut writer)?;

    let mut encoder = GzEncoder::new(Vec::new(), FlateCompression::default());
    encoder.write_all(&writer.into_bytes())?;
    let compressed = encoder.finish()?;

    let mut file = File::create(path)?;
    file.write_all(&compressed)?;
    Ok(())
}

/// 构造玩家数据 NBT（用于 level.dat `Player` 与 playerdata）。
#[must_use]
pub fn player_nbt(
    name: &str,
    uuid: &uuid::Uuid,
    dimension: &str,
    pos: [f64; 3],
    rotation: [f32; 2],
    game_mode: u8,
) -> Nbt {
    let mut player = NbtCompound::new();
    player.insert("DataVersion", Nbt::Int(DATA_VERSION));
    player.insert("Dimension", Nbt::String(dimension.to_owned()));

    let mut pos_list = NbtList::empty();
    pos_list.tag = TagId::Double;
    for value in pos {
        pos_list.values.push(Nbt::Double(value));
    }
    player.insert("Pos", Nbt::List(Box::new(pos_list)));

    let mut rot_list = NbtList::empty();
    rot_list.tag = TagId::Float;
    for value in rotation {
        rot_list.values.push(Nbt::Float(value));
    }
    player.insert("Rotation", Nbt::List(Box::new(rot_list)));

    let mut abilities = NbtCompound::new();
    abilities.insert("invulnerable", Nbt::Byte(0));
    abilities.insert("flying", Nbt::Byte(0));
    abilities.insert("mayfly", Nbt::Byte(0));
    abilities.insert("instabuild", Nbt::Byte(0));
    abilities.insert("mayBuild", Nbt::Byte(1));
    abilities.insert("flySpeed", Nbt::Float(0.05));
    abilities.insert("walkSpeed", Nbt::Float(0.1));
    player.insert("abilities", Nbt::Compound(Box::new(abilities)));

    player.insert("playerGameType", Nbt::Int(i32::from(game_mode)));
    player.insert("SpawnX", Nbt::Int(0));
    player.insert("SpawnY", Nbt::Int(4));
    player.insert("SpawnZ", Nbt::Int(0));
    player.insert(
        "SpawnDimension",
        Nbt::String("minecraft:overworld".to_owned()),
    );
    player.insert("SpawnAngle", Nbt::Float(0.0));
    player.insert("SpawnForced", Nbt::Byte(1));
    player.insert("seenCredits", Nbt::Byte(1));

    let mut tags = NbtCompound::new();
    tags.insert("name", Nbt::String(name.to_owned()));
    tags.insert("uuid", Nbt::IntArray(uuid_ints(uuid).into_boxed_slice()));
    player.insert("bukkit", Nbt::Compound(Box::new(tags)));
    player.insert("XpLevel", Nbt::Int(0));
    player.insert("XpP", Nbt::Float(0.0));
    player.insert("XpSeed", Nbt::Int(0));
    player.insert("XpTotal", Nbt::Int(0));
    player.insert("foodExhaustionLevel", Nbt::Float(0.0));
    player.insert("foodLevel", Nbt::Int(20));
    player.insert("foodSaturationLevel", Nbt::Float(5.0));
    player.insert("foodTickTimer", Nbt::Int(0));
    player.insert("Health", Nbt::Float(20.0));
    player.insert("HurtTime", Nbt::Short(0));
    player.insert("DeathTime", Nbt::Short(0));
    player.insert("OnGround", Nbt::Byte(1));
    player.insert("Motion", Nbt::List(Box::new(NbtList::empty())));
    player.insert("fire", Nbt::Short(-1));
    player.insert("air", Nbt::Short(300));
    player.insert("FallDistance", Nbt::Float(0.0));
    player.insert("Invulnerable", Nbt::Byte(0));
    player.insert("Sleeping", Nbt::Byte(0));
    player.insert("SelectedItemSlot", Nbt::Int(0));
    player.insert("Score", Nbt::Int(0));
    Nbt::Compound(Box::new(player))
}

/// UUID → 4 个 i32（Java `IntBuffer` 顺序）。
fn uuid_ints(uuid: &uuid::Uuid) -> Vec<i32> {
    let bytes = uuid.as_bytes();
    bytes
        .chunks_exact(4)
        .map(|chunk| i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{read_level_dat, write_level_dat};
    use crate::level::LevelData;

    #[test]
    fn level_dat_round_trip() {
        let path = std::env::temp_dir().join(format!("level-{}.dat", std::process::id()));
        let data = LevelData {
            level_name: "test_world".to_owned(),
            gamerules: vec![
                ("doDaylightCycle".to_owned(), "true".to_owned()),
                ("keepInventory".to_owned(), "false".to_owned()),
            ],
            player: None,
        };
        write_level_dat(&path, &data).unwrap();
        let read = read_level_dat(&path).unwrap();
        assert_eq!(read.level_name, "test_world");
        assert_eq!(read.gamerules.len(), 2);
        assert!(
            read.gamerules
                .contains(&("keepInventory".to_owned(), "false".to_owned()))
        );
        std::fs::remove_file(&path).unwrap();
    }
}
