//! 1.21.1 Anvil 存档格式（DataVersion 3955）。
//!
//! 提供 .mca region 文件读写（扇区分配、压缩编解码，含 LZ4 与外部
//! .mcc 流）、chunk NBT 编解码、level.dat 与 playerdata 读写。
