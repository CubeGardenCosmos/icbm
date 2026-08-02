//! 1.21.1 Anvil 存档格式（DataVersion 3955）。
//!
//! - [`region_file`]：.mca region 文件（8KiB 头部、扇区分配、压缩编解码，
//!   含 LZ4 与外部 .mcc 流）
//! - [`chunk`]：chunk NBT 编解码（对照 `ChunkSerializer.java`）
//! - [`level`]：level.dat 读写
//! - [`player`]：playerdata/\<uuid\>.dat 读写

pub mod chunk;
pub mod level;
pub mod player;
pub mod region_file;

pub use region_file::{REGION_SIZE, RegionFile, RegionFileError};
