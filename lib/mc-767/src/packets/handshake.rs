//! 握手包。

use crate::codec::{CodecError, Reader};

use super::handshake_ids::INTENTION;

/// Serverbound：握手意图（0x00）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SIntent {
    /// 客户端协议版本。
    pub protocol_version: i32,
    /// 服务器地址（原样回显，不用于连接）。
    pub server_address: String,
    /// 服务器端口。
    pub server_port: u16,
    /// 下一状态：1=状态，2=登录，3=转移。
    pub next_state: i32,
}

impl SIntent {
    /// 解码握手包载荷。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            protocol_version: reader.read_var_int()?,
            server_address: reader.read_string()?,
            server_port: reader.read_u16_be()?,
            next_state: reader.read_var_int()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        INTENTION
    }
}

#[cfg(test)]
mod tests {
    use super::SIntent;
    use crate::codec::{Reader, Writer};

    #[test]
    fn decode_intention() {
        let mut w = Writer::new();
        w.write_var_int(767);
        w.write_string("localhost");
        w.write_u16_be(25565);
        w.write_var_int(2);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let intent = SIntent::decode(&mut r).unwrap();
        assert_eq!(intent.protocol_version, 767);
        assert_eq!(intent.server_address, "localhost");
        assert_eq!(intent.server_port, 25565);
        assert_eq!(intent.next_state, 2);
        assert!(r.is_empty());
    }
}
