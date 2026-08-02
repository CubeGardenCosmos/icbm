//! 登录阶段包。

use uuid::Uuid;

use crate::codec::{CodecError, Reader, Writer};

use super::login_ids::{
    CLIENTBOUND_COMPRESSION, CLIENTBOUND_DISCONNECT, CLIENTBOUND_HELLO, CLIENTBOUND_SUCCESS,
    SERVERBOUND_ACKNOWLEDGED, SERVERBOUND_KEY, SERVERBOUND_START,
};

/// Serverbound：登录开始（0x00，1.21.1：`[String name≤16][UUID profile_id]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SLoginStart {
    /// 玩家名。
    pub name: String,
    /// 玩家 UUID（客户端提供；离线模式下服务器以 `OfflinePlayer:` 哈希为准）。
    pub profile_id: Uuid,
}

impl SLoginStart {
    /// 解码（对照 `LoginHelloC2SPacket.java`）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        let name = reader.read_string()?;
        let profile_id = reader.read_uuid()?;
        Ok(Self { name, profile_id })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_START
    }
}

/// Serverbound：登录确认（0x03，空载荷；客户端收到登录成功后发送）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SLoginAcknowledged;

impl SLoginAcknowledged {
    /// 解码（空载荷）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        debug_assert!(reader.is_empty());
        Ok(Self)
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_ACKNOWLEDGED
    }
}

/// Clientbound：加密请求（0x01 `LoginHelloS2CPacket`）。
///
/// 字段（对照 `LoginHelloS2CPacket.java`）：`String server_id(≤20)` →
/// `ByteArray RSA 公钥` → `ByteArray verify token` → `Bool needs_authentication`。
/// 正版验证时 `server_id` 为空串、`needs_authentication` 为 true（客户端因此走
/// session server 认证流程）；离线模式下也可用加密但 `needs_authentication` 为 false。
#[derive(Debug, Clone)]
pub struct CEncryptionRequest {
    /// server id（正版验证为空串）。
    pub server_id: String,
    /// RSA 公钥（SPKI DER）。
    pub public_key: Vec<u8>,
    /// 验证令牌（随机 4 字节）。
    pub verify_token: Vec<u8>,
    /// 是否要求 Mojang 认证（正版模式）。
    pub needs_authentication: bool,
}

impl CEncryptionRequest {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_string(&self.server_id);
        writer.write_byte_array(&self.public_key);
        writer.write_byte_array(&self.verify_token);
        writer.write_bool(self.needs_authentication);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_HELLO
    }
}

/// Serverbound：加密响应（0x01 `LoginKeyC2SPacket`）。
///
/// 字段（对照 `LoginKeyC2SPacket.java`）：`ByteArray 加密共享密钥` →
/// `ByteArray 加密 verify token`（均为 RSA 公钥加密）。
#[derive(Debug, Clone)]
pub struct SEncryptionResponse {
    /// RSA 加密的共享密钥。
    pub encrypted_secret: Vec<u8>,
    /// RSA 加密的 verify token。
    pub encrypted_token: Vec<u8>,
}

impl SEncryptionResponse {
    /// 解码（对照 `LoginKeyC2SPacket.java`）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            encrypted_secret: reader.read_byte_array()?.to_vec(),
            encrypted_token: reader.read_byte_array()?.to_vec(),
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_KEY
    }
}

/// Clientbound：登录成功（0x02）。
#[derive(Debug, Clone)]
pub struct CLoginSuccess {
    /// 玩家 UUID。
    pub uuid: Uuid,
    /// 玩家名。
    pub name: String,
    /// 严格错误处理（1.21 起为 true）。
    pub strict_error_handling: bool,
}

impl CLoginSuccess {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_uuid(&self.uuid);
        writer.write_string(&self.name);
        writer.write_var_int(0); // 属性数量（离线模式为空）
        writer.write_bool(self.strict_error_handling);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_SUCCESS
    }
}

/// Clientbound：登录断开（0x00）。
#[derive(Debug, Clone)]
pub struct CLoginDisconnect {
    /// 断开原因纯文本（组件网络序列化为 NBT）。
    pub reason: String,
}

impl CLoginDisconnect {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_text_component(&self.reason);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_DISCONNECT
    }
}

/// Clientbound：设置压缩（0x03）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CSetCompression {
    /// 压缩阈值（此后所有帧启用压缩）。
    pub threshold: i32,
}

impl CSetCompression {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_var_int(self.threshold);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_COMPRESSION
    }
}

#[cfg(test)]
mod tests {
    use super::{CLoginSuccess, SLoginStart};
    use crate::codec::{Reader, Writer};
    use uuid::Uuid;

    #[test]
    fn login_start_decodes_name_and_uuid() {
        let uuid = Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        let mut w = Writer::new();
        w.write_string("steve");
        w.write_uuid(&uuid);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let start = SLoginStart::decode(&mut r).unwrap();
        assert_eq!(start.name, "steve");
        assert_eq!(start.profile_id, uuid);
        assert!(r.is_empty());
    }

    #[test]
    fn login_success_encoding() {
        let uuid = Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        let packet = CLoginSuccess {
            uuid,
            name: "steve".to_owned(),
            strict_error_handling: true,
        };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        // 16 + varint(5)+5 + varint(0) + bool(1)
        assert_eq!(bytes.len(), 16 + 6 + 1 + 1);
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_uuid().unwrap(), uuid);
        assert_eq!(r.read_string().unwrap(), "steve");
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert!(r.read_bool().unwrap());
    }
}
