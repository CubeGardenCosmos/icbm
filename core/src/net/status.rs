//! 状态查询：MOTD JSON 与 favicon。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use icbm_mc_767::packets::status::{PlayerSample, Players, StatusResponse, Version};

/// 内置默认 favicon（64×64 PNG，`data:image/png;base64,` 数据 URL）。
#[must_use]
pub fn default_favicon() -> Option<String> {
    const PNG: &[u8] = include_bytes!("../../../assets/icbmlogo.png");
    const PREFIX: &str = "data:image/png;base64,";
    let encoded = BASE64.encode(PNG);
    Some(format!("{PREFIX}{encoded}"))
}

/// 构造状态响应。
#[must_use]
pub fn status_response(
    motd: &str,
    max_players: i32,
    online: i32,
    sample_names: &[String],
) -> StatusResponse {
    let sample = (!sample_names.is_empty()).then(|| {
        sample_names
            .iter()
            .take(12)
            .map(|name| PlayerSample {
                name: name.clone(),
                id: "00000000-0000-0000-0000-000000000000".to_owned(),
            })
            .collect()
    });
    StatusResponse {
        version: Version {
            protocol: icbm_mc_767::protocol::PROTOCOL_VERSION,
            name: icbm_mc_767::protocol::MC_VERSION.to_owned(),
        },
        players: Players {
            max: max_players,
            online,
            sample,
        },
        description: serde_json::json!({ "text": motd }),
        favicon: default_favicon(),
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE64, default_favicon, status_response};
    use base64::Engine;

    #[test]
    fn favicon_is_base64_png() {
        let favicon = default_favicon().unwrap();
        assert!(favicon.starts_with("data:image/png;base64,"));
        let data = favicon.trim_start_matches("data:image/png;base64,");
        let decoded = BASE64.decode(data).unwrap();
        assert_eq!(&decoded[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn status_json_fields() {
        let response = status_response("test motd", 20, 1, &["steve".to_owned()]);
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"protocol\":767"));
        assert!(json.contains("\"name\":\"1.21.1\""));
        assert!(json.contains("\"test motd\""));
        assert!(json.contains("data:image/png;base64,"));
    }
}
