//! .mca region 文件读写。
//!
//! 格式（对照 `RegionFile.java`）：
//! - 8KiB 头部：位置表（1024 × u32 BE：偏移扇区数 `offset << 8 | size`）+
//!   时间戳表（1024 × u32 BE）
//! - 区块数据：`[u32 长度][u8 压缩标志][数据]`，长度 = 数据字节数 + 1
//! - 压缩标志：位 7 表示外部 .mcc 流；低 7 位 = 压缩类型
//!   （1=gzip，2=zlib，3=无，4=LZ4）
//!
//! 生命周期：`open`（独占 flock）→ `read_chunk`/`write_chunk`（内存态）→
//! `flush`（按索引顺序 bump 分配扇区并整体重写头部与数据）→ `drop` 解锁。

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::Compression as FlateCompression;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};

use crate::chunk::{ChunkNbt, ChunkReadError};

/// region 每边区块数（32×32）。
pub const REGION_SIZE: usize = 32;
/// 区块索引总数。
pub const CHUNK_COUNT: usize = REGION_SIZE * REGION_SIZE;
/// 扇区字节数。
pub const SECTOR_BYTES: usize = 4096;
/// 区块数据内联字节数上限（超出写外部 .mcc）。
const MAX_INTERNAL_BYTES: usize = 1024 * SECTOR_BYTES;

/// 压缩类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Compression {
    GZip = 1,
    ZLib = 2,
    None = 3,
    LZ4 = 4,
}

impl Compression {
    /// 从标志字节解析（低 7 位）。
    const fn from_flags(flags: u8) -> Option<Self> {
        match flags & 0x7F {
            1 => Some(Self::GZip),
            2 => Some(Self::ZLib),
            3 => Some(Self::None),
            4 => Some(Self::LZ4),
            _ => None,
        }
    }

    /// 压缩字节。
    const fn id(self) -> u8 {
        self as u8
    }

    /// 压缩数据。
    fn compress(self, data: &[u8]) -> Result<Vec<u8>, RegionFileError> {
        match self {
            Self::GZip => {
                let mut encoder = GzEncoder::new(Vec::new(), FlateCompression::default());
                encoder.write_all(data).map_err(RegionFileError::Compress)?;
                encoder.finish().map_err(RegionFileError::Compress)
            }
            Self::ZLib => {
                let mut encoder = ZlibEncoder::new(Vec::new(), FlateCompression::default());
                encoder.write_all(data).map_err(RegionFileError::Compress)?;
                encoder.finish().map_err(RegionFileError::Compress)
            }
            Self::None => Ok(data.to_vec()),
            Self::LZ4 => {
                use lz4_java_wrc::{Context, Lz4BlockOutput};
                let mut out = Vec::new();
                let block_size = 1 << 16;
                let mut encoder =
                    Lz4BlockOutput::with_context(&mut out, Context::default(), block_size)
                        .map_err(RegionFileError::Compress)?;
                encoder.write_all(data).map_err(RegionFileError::Compress)?;
                drop(encoder);
                Ok(out)
            }
        }
    }

    /// 解压数据。
    fn decompress(self, data: &[u8]) -> Result<Vec<u8>, RegionFileError> {
        match self {
            Self::GZip => {
                let mut decoder = GzDecoder::new(data);
                let mut out = Vec::new();
                decoder
                    .read_to_end(&mut out)
                    .map_err(RegionFileError::Decompress)?;
                Ok(out)
            }
            Self::ZLib => {
                let mut decoder = ZlibDecoder::new(data);
                let mut out = Vec::new();
                decoder
                    .read_to_end(&mut out)
                    .map_err(RegionFileError::Decompress)?;
                Ok(out)
            }
            Self::None => Ok(data.to_vec()),
            Self::LZ4 => {
                use lz4_java_wrc::Lz4BlockInput;
                let mut decoder = Lz4BlockInput::new(data);
                let mut out = Vec::new();
                decoder
                    .read_to_end(&mut out)
                    .map_err(RegionFileError::Decompress)?;
                Ok(out)
            }
        }
    }
}

/// .mca 读写错误。
#[derive(Debug)]
pub enum RegionFileError {
    /// 底层 IO 错误。
    Io(std::io::Error),
    /// 压缩失败。
    Compress(std::io::Error),
    /// 解压失败。
    Decompress(std::io::Error),
    /// 区块数据损坏。
    Corrupt(String),
    /// 区块 NBT 解析失败。
    Chunk(ChunkReadError),
    /// 文件已被占用（锁失败）。
    Locked,
}

impl fmt::Display for RegionFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "io: {err}"),
            Self::Compress(err) => write!(f, "compress: {err}"),
            Self::Decompress(err) => write!(f, "decompress: {err}"),
            Self::Corrupt(msg) => write!(f, "corrupt region file: {msg}"),
            Self::Chunk(err) => write!(f, "chunk: {err}"),
            Self::Locked => f.write_str("region file is locked by another process"),
        }
    }
}

impl std::error::Error for RegionFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) | Self::Compress(err) | Self::Decompress(err) => Some(err),
            Self::Chunk(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for RegionFileError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<ChunkReadError> for RegionFileError {
    fn from(value: ChunkReadError) -> Self {
        Self::Chunk(value)
    }
}

/// 区块在 region 内的局部坐标（0..32）。
#[must_use]
pub const fn local_coord(global_chunk: i32) -> usize {
    (global_chunk & (REGION_SIZE as i32 - 1)) as usize
}

/// 区块索引（`(local_x << 5) | local_z`，与 vanilla 一致）。
#[must_use]
pub const fn chunk_index(local_x: usize, local_z: usize) -> usize {
    (local_x << 5) | local_z
}

/// 已打开的 .mca region 文件（独占锁）。
pub struct RegionFile {
    path: PathBuf,
    file: File,
    /// 位置表（offset<<8|size，0 = 空）。
    locations: [u32; CHUNK_COUNT],
    /// 时间戳表。
    timestamps: [u32; CHUNK_COUNT],
    /// 内存态区块（原始 NBT 字节；覆盖文件内容直至 flush）。
    chunks: Vec<Option<Vec<u8>>>,
}

impl RegionFile {
    /// 打开（不存在则创建）并加独占锁。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RegionFileError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        lock_exclusive(&file)?;

        let mut locations = [0u32; CHUNK_COUNT];
        let mut timestamps = [0u32; CHUNK_COUNT];
        let mut header = [0u8; 2 * SECTOR_BYTES];
        let read = (&file).read(&mut header)?;
        if read == 2 * SECTOR_BYTES {
            for (i, entry) in locations.iter_mut().enumerate() {
                let off = i * 4;
                *entry = u32::from_be_bytes([
                    header[off],
                    header[off + 1],
                    header[off + 2],
                    header[off + 3],
                ]);
            }
            for (i, entry) in timestamps.iter_mut().enumerate() {
                let off = SECTOR_BYTES + i * 4;
                *entry = u32::from_be_bytes([
                    header[off],
                    header[off + 1],
                    header[off + 2],
                    header[off + 3],
                ]);
            }
        }

        Ok(Self {
            path,
            file,
            locations,
            timestamps,
            chunks: vec![None; CHUNK_COUNT],
        })
    }

    /// region 文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 区块是否已存在（文件或内存态）。
    #[must_use]
    pub fn has_chunk(&self, index: usize) -> bool {
        self.chunks.get(index).is_some_and(Option::is_some)
            || self.locations.get(index).is_some_and(|&loc| loc != 0)
    }

    /// 是否有关闭前未落盘的修改。
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.chunks.iter().any(Option::is_some)
    }

    /// 读取区块 NBT（不存在返回 `Ok(None)`）。
    pub fn read_chunk(&mut self, index: usize) -> Result<Option<ChunkNbt>, RegionFileError> {
        let Some(raw) = self.read_raw_chunk(index)? else {
            return Ok(None);
        };
        ChunkNbt::from_bytes(&raw).map(Some).map_err(From::from)
    }

    /// 写入区块 NBT（内存态，需 `flush` 落盘）。
    pub fn write_chunk(&mut self, index: usize, chunk: &ChunkNbt) -> Result<(), RegionFileError> {
        self.chunks[index] = Some(chunk.to_bytes()?);
        self.timestamps[index] = now_epoch_seconds();
        Ok(())
    }

    /// 删除区块（内存态）。
    pub fn delete_chunk(&mut self, index: usize) {
        if let Some(slot) = self.chunks.get_mut(index) {
            *slot = None;
        }
    }

    /// 读取未压缩的原始 NBT 字节。
    fn read_raw_chunk(&mut self, index: usize) -> Result<Option<Vec<u8>>, RegionFileError> {
        if index >= CHUNK_COUNT {
            return Err(RegionFileError::Corrupt(
                "chunk index out of range".to_owned(),
            ));
        }
        if let Some(raw) = &self.chunks[index] {
            return Ok(Some(raw.clone()));
        }
        // 继续读文件
        let location = self.locations[index];
        if location == 0 {
            return Ok(None);
        }
        let offset = (location >> 8) as usize;
        let size = (location & 0xFF) as usize;
        let mut buf = vec![0u8; size * SECTOR_BYTES];
        self.file
            .seek(SeekFrom::Start((offset * SECTOR_BYTES) as u64))?;
        self.file.read_exact(&mut buf)?;

        if buf.len() < 5 {
            return Err(RegionFileError::Corrupt(
                "chunk header truncated".to_owned(),
            ));
        }
        let declared_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let flags = buf[4];
        let body = &buf[5..];

        if flags & 0x80 != 0 {
            // 外部 .mcc 流
            let ext_data = std::fs::read(self.external_chunk_path(index))?;
            let compression = Compression::from_flags(flags)
                .ok_or_else(|| RegionFileError::Corrupt("unknown compression".to_owned()))?;
            return compression.decompress(&ext_data).map(Some);
        }

        if declared_len == 0 {
            return Ok(None);
        }
        if declared_len - 1 > body.len() {
            return Err(RegionFileError::Corrupt("chunk data truncated".to_owned()));
        }
        let compression = Compression::from_flags(flags)
            .ok_or_else(|| RegionFileError::Corrupt("unknown compression".to_owned()))?;
        compression.decompress(&body[..declared_len - 1]).map(Some)
    }

    fn external_chunk_path(&self, index: usize) -> PathBuf {
        let x = (index >> 5) as i32;
        let z = (index & 0x1F) as i32;
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("c.{x}.{z}.mcc"))
    }

    /// 落盘：按索引顺序 bump 分配扇区，整体重写头部与数据。
    pub fn flush(&mut self) -> Result<(), RegionFileError> {
        let mut sector_data: Vec<Vec<u8>> = Vec::new();
        let mut next_sector = 2usize;
        let mut locations = [0u32; CHUNK_COUNT];

        for (index, entry) in self.chunks.iter().enumerate() {
            let Some(raw) = entry else { continue };
            let compressed = Compression::LZ4.compress(raw)?;
            let total_len = compressed.len() + 5;
            let internal = total_len <= MAX_INTERNAL_BYTES;

            let (offset, size) = if internal {
                let sector_count = total_len.div_ceil(SECTOR_BYTES);
                let offset = next_sector;
                next_sector += sector_count;
                let mut buf = vec![0u8; sector_count * SECTOR_BYTES];
                buf[0..4].copy_from_slice(
                    &u32::try_from(compressed.len() + 1)
                        .expect("len")
                        .to_be_bytes(),
                );
                buf[4] = Compression::LZ4.id();
                buf[5..5 + compressed.len()].copy_from_slice(&compressed);
                sector_data.push(buf);
                (offset, sector_count as u32)
            } else {
                // 外部 .mcc：头部留 1 个扇区记录 [0][0x80|id]
                let offset = next_sector;
                next_sector += 1;
                let mut buf = vec![0u8; SECTOR_BYTES];
                buf[4] = 0x80 | Compression::LZ4.id();
                sector_data.push(buf);
                let ext = self.external_chunk_path(index);
                std::fs::write(ext, &compressed)?;
                (offset, 1)
            };
            locations[index] = ((offset as u32) << 8) | size;
        }

        let mut header = vec![0u8; 2 * SECTOR_BYTES];
        for (i, entry) in locations.iter().enumerate() {
            let off = i * 4;
            header[off..off + 4].copy_from_slice(&entry.to_be_bytes());
        }
        for (i, entry) in self.timestamps.iter().enumerate() {
            let off = SECTOR_BYTES + i * 4;
            header[off..off + 4].copy_from_slice(&entry.to_be_bytes());
        }

        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        for (i, data) in sector_data.iter().enumerate() {
            self.file
                .seek(SeekFrom::Start(((2 + i) * SECTOR_BYTES) as u64))?;
            self.file.write_all(data)?;
        }
        self.file.sync_all()?;
        self.locations = locations;
        self.chunks = vec![None; CHUNK_COUNT];
        Ok(())
    }
}

impl Drop for RegionFile {
    fn drop(&mut self) {
        let _ = unlock(&self.file);
    }
}

/// 对文件加独占 flock（非阻塞；同一进程内不同 fd 也会互斥）。
fn lock_exclusive(file: &File) -> Result<(), RegionFileError> {
    use std::os::unix::io::AsRawFd;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Ok(())
    } else {
        match std::io::Error::last_os_error().kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => {
                Err(RegionFileError::Locked)
            }
            other => Err(RegionFileError::Io(std::io::Error::new(
                other,
                "flock failed",
            ))),
        }
    }
}

/// 释放文件锁。
fn unlock(file: &File) -> Result<(), RegionFileError> {
    use std::os::unix::io::AsRawFd;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if ret == 0 {
        Ok(())
    } else {
        Err(RegionFileError::Io(std::io::Error::last_os_error()))
    }
}

/// 当前 Unix 时间（秒）。
fn now_epoch_seconds() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u32::try_from(d.as_secs()).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use super::{Compression, RegionFile, chunk_index, local_coord};
    use crate::chunk::{ChunkNbt, SectionNbt};

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
                block_data: Vec::new(),
                biome_palette: vec!["minecraft:plains".to_owned()],
                biome_data: Vec::new(),
                sky_light: (sy >= 0).then(|| vec![0xFFu8; 2048]),
                block_light: None,
            });
        }
        ChunkNbt::new(x, z, -4, sections)
    }

    #[test]
    fn compression_round_trip_all() {
        let data = b"hello anvil world".repeat(100);
        for c in [
            Compression::GZip,
            Compression::ZLib,
            Compression::None,
            Compression::LZ4,
        ] {
            let compressed = c.compress(&data).unwrap();
            let decompressed = c.decompress(&compressed).unwrap();
            assert_eq!(decompressed, data, "{c:?} round trip");
        }
    }

    #[test]
    fn index_math() {
        assert_eq!(local_coord(0), 0);
        assert_eq!(local_coord(31), 31);
        assert_eq!(local_coord(32), 0);
        assert_eq!(chunk_index(0, 0), 0);
        assert_eq!(chunk_index(31, 31), 1023);
    }

    #[test]
    fn region_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("anvil-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.0.0.mca");

        let mut file = RegionFile::open(&path).unwrap();
        file.write_chunk(0, &sample_chunk(0, 0)).unwrap();
        file.write_chunk(chunk_index(5, 5), &sample_chunk(5, 5))
            .unwrap();
        file.flush().unwrap();
        drop(file);

        let mut reopened = RegionFile::open(&path).unwrap();
        assert!(reopened.has_chunk(0));
        let read = reopened.read_chunk(0).unwrap().unwrap();
        assert_eq!(read.x, 0);
        assert_eq!(read.z, 0);
        assert_eq!(read.sections.len(), 24);
        assert_eq!(read.sections[0].block_palette, vec!["minecraft:bedrock"]);
        let read2 = reopened.read_chunk(chunk_index(5, 5)).unwrap().unwrap();
        assert_eq!(read2.x, 5);
        assert_eq!(read2.z, 5);
        assert!(reopened.read_chunk(1).unwrap().is_none());
        drop(reopened);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_is_locked() {
        let dir = std::env::temp_dir().join(format!("anvil-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.0.0.mca");
        let _first = RegionFile::open(&path).unwrap();
        let second = RegionFile::open(&path);
        assert!(second.is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
