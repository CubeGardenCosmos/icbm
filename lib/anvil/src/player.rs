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
use icbm_mc_767::nbt::{Nbt, decode_named, encode_named};
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
}
