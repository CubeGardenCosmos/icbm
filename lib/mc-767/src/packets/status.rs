//! 状态阶段包（MOTD 查询与 ping/pong）。

use serde::{Deserialize, Serialize};

use crate::codec::{CodecError, Reader, Writer};

use super::status_ids::{
    CLIENTBOUND_PONG, CLIENTBOUND_RESPONSE, SERVERBOUND_PING, SERVERBOUND_REQUEST,
};

/// Serverbound：状态请求（0x00，无载荷）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SStatusRequest;

impl SStatusRequest {
    /// 解码（空载荷）。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        debug_assert!(reader.is_empty());
        Ok(Self)
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_REQUEST
    }
}

/// Serverbound：ping 请求（0x01）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SPingRequest {
    /// 随机载荷，需原样回显。
    pub payload: i64,
}

impl SPingRequest {
    /// 解码。
    pub fn decode(reader: &mut Reader<'_>) -> Result<Self, CodecError> {
        Ok(Self {
            payload: reader.read_i64_be()?,
        })
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        SERVERBOUND_PING
    }
}

/// 状态响应 JSON（`version`/`players`/`description`/`favicon`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    /// 版本信息。
    pub version: Version,
    /// 玩家信息。
    pub players: Players,
    /// MOTD 文本组件 JSON。
    pub description: serde_json::Value,
    /// 可选：`data:image/png;base64,` favicon。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
}

/// 状态响应版本字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    /// 协议版本（767）。
    pub protocol: i32,
    /// 版本名。
    pub name: String,
}

/// 状态响应玩家字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Players {
    /// 最大玩家数。
    pub max: i32,
    /// 在线玩家数。
    pub online: i32,
    /// 样例玩家（最多 12）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample: Option<Vec<PlayerSample>>,
}

/// 状态响应样例玩家。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerSample {
    /// 玩家名。
    pub name: String,
    /// 玩家 UUID。
    pub id: String,
}

/// Clientbound：状态响应（0x00）。
#[derive(Debug, Clone)]
pub struct CStatusResponse {
    /// 响应 JSON。
    pub response: StatusResponse,
}

impl CStatusResponse {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        let json = serde_json::to_string(&self.response).expect("status json must serialize");
        writer.write_string(&json);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_RESPONSE
    }
}

/// Clientbound：pong 响应（0x01）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CPongResponse {
    /// 原样回显的 ping 载荷。
    pub payload: i64,
}

impl CPongResponse {
    /// 编码载荷。
    pub fn encode(&self, writer: &mut Writer) {
        writer.write_i64_be(self.payload);
    }

    /// 包 ID。
    #[must_use]
    pub const fn packet_id() -> i32 {
        CLIENTBOUND_PONG
    }
}

#[cfg(test)]
mod tests {
    use super::{CPongResponse, CStatusResponse, PlayerSample, Players, StatusResponse, Version};
    use crate::codec::{Reader, Writer};

    #[test]
    fn status_response_json() {
        let response = StatusResponse {
            version: Version {
                protocol: 767,
                name: "1.21.1".to_owned(),
            },
            players: Players {
                max: 20,
                online: 1,
                sample: Some(vec![PlayerSample {
                    name: "steve".to_owned(),
                    id: "00000000-0000-0000-0000-000000000001".to_owned(),
                }]),
            },
            description: serde_json::json!({"text": "Hello"}),
            favicon: Some("data:image/png;base64,AA==".to_owned()),
        };
        let packet = CStatusResponse { response };
        let mut w = Writer::new();
        packet.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        let json = r.read_string().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"]["protocol"], 767);
        assert_eq!(parsed["players"]["online"], 1);
        assert!(json.contains("data:image/png;base64,AA=="));
    }

    #[test]
    fn pong_round_trip() {
        let mut w = Writer::new();
        CPongResponse { payload: 12345 }.encode(&mut w);
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes);
        assert_eq!(r.read_i64_be().unwrap(), 12345);
    }
}
