//! Minecraft Java 1.21.1（协议 767）网络协议与 NBT 编解码。
//!
//! 分层：
//! - [`codec`]：基础读写原语（VarInt/VarLong/字符串/UUID/BitSet 等）
//! - [`nbt`]：NBT 标签模型与命名/未命名格式编解码
//! - [`packets`]：包 ID、帧编解码（可选 zlib 压缩）、各状态的最小包集
//!
//! 版本快照：协议 767、DataVersion 3955（见 [`protocol`]）。

pub mod codec;
pub mod nbt;
pub mod packets;

/// 固定版本快照（对照 `net.minecraft.SharedConstants` 与 master `util/src/version.rs`）。
pub mod protocol {
    /// 网络协议版本（Java 1.21 与 1.21.1 共用 767，实现以 1.21.1 为准）。
    pub const PROTOCOL_VERSION: i32 = 767;
    /// 客户端可见的版本名。
    pub const MC_VERSION: &str = "1.21.1";
    /// 世界/数据版本（Anvil 存档）。
    pub const DATA_VERSION: i32 = 3955;
    /// 资源包格式版本。
    pub const RESOURCE_PACK_VERSION: i32 = 34;
    /// 数据包格式版本。
    pub const DATA_PACK_VERSION: i32 = 48;
}

/// 连接状态（对应握手 `next_state` 与登录/配置/Play 流转）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Handshake,
    Status,
    Login,
    Config,
    Play,
}
