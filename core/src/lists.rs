//! 白名单与封禁列表（`whitelist.json` / `banned-players.json`，参考 master
//! `config/src/whitelist.rs` 与 `core/src/data/whitelist.rs`）。
//!
//! 存储为 JSON 数组，每项 `{"uuid": "<uuid>", "name": "<name>"}`；白名单启用
//! 时登录阶段检查，封禁列表登录时拒绝。文件按世界目录存放（`<world>/whitelist.json`）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 名单条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListEntry {
    /// 玩家 UUID。
    pub uuid: Uuid,
    /// 玩家名。
    pub name: String,
}

/// 名单存储（白名单或封禁列表共用）。
#[derive(Debug, Clone, Default)]
pub struct NameList {
    path: PathBuf,
    entries: Vec<ListEntry>,
}

impl NameList {
    /// 从文件加载（不存在时为空列表）。
    pub fn load(world_dir: impl AsRef<Path>, filename: &str) -> Self {
        let path = world_dir.as_ref().join(filename);
        let entries = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    /// 是否包含该玩家（按 UUID 或名字，大小写不敏感）。
    #[must_use]
    pub fn contains(&self, uuid: &Uuid, name: &str) -> bool {
        self.entries.iter().any(|entry| {
            entry.uuid == *uuid || entry.name.eq_ignore_ascii_case(name)
        })
    }

    /// 添加玩家并持久化。
    pub fn add(&mut self, uuid: Uuid, name: &str) -> Result<(), String> {
        if !self.contains(&uuid, name) {
            self.entries.push(ListEntry {
                uuid,
                name: name.to_owned(),
            });
        }
        self.save()
    }

    /// 移除玩家（按名字或 UUID）并持久化；返回是否移除成功。
    pub fn remove(&mut self, uuid: &Uuid, name: &str) -> Result<bool, String> {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.uuid != *uuid && !entry.name.eq_ignore_ascii_case(name)
        });
        let removed = self.entries.len() < before;
        self.save()?;
        Ok(removed)
    }

    /// 条目数。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 条目列表（查询用）。
    #[must_use]
    pub fn entries(&self) -> &[ListEntry] {
        &self.entries
    }

    /// 持久化到 JSON。
    fn save(&self) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.entries).map_err(|err| err.to_string())?;
        std::fs::write(&self.path, json).map_err(|err| err.to_string())
    }
}

/// 登录名单状态（core 持有，白名单/封禁查询用）。
#[derive(Debug, Clone, Default)]
pub struct LoginLists {
    /// 白名单。
    pub whitelist: NameList,
    /// 封禁列表。
    pub banned: NameList,
    /// 是否启用白名单（`whitelist on/off`）。
    pub whitelist_enabled: bool,
}

impl LoginLists {
    /// 从世界目录加载。
    #[must_use]
    pub fn load(world_dir: impl AsRef<Path>) -> Self {
        Self {
            whitelist: NameList::load(&world_dir, "whitelist.json"),
            banned: NameList::load(&world_dir, "banned-players.json"),
            whitelist_enabled: false,
        }
    }

    /// 登录检查：封禁拒绝、白名单（启用时）拒绝未列入者。返回 `Ok(())` 或拒绝原因。
    pub fn check_login(&self, uuid: &Uuid, name: &str) -> Result<(), String> {
        if self.banned.contains(uuid, name) {
            return Err("你已被封禁".to_owned());
        }
        if self.whitelist_enabled && !self.whitelist.contains(uuid, name) {
            return Err("你不在白名单中".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LoginLists, NameList};
    use uuid::Uuid;

    #[test]
    fn whitelist_round_trip() {
        let dir = std::env::temp_dir().join(format!("icbm-lists-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let uuid = Uuid::parse_str("069a79f444e94726a5befca90e38aaf5").unwrap();
        let mut list = NameList::load(&dir, "whitelist.json");
        list.add(uuid, "Notch").unwrap();
        assert!(list.contains(&uuid, "Notch"));
        assert!(list.contains(&uuid, "notch")); // 大小写不敏感
        assert_eq!(list.len(), 1);

        // 重新加载
        let reloaded = NameList::load(&dir, "whitelist.json");
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.contains(&uuid, "Notch"));

        // 移除
        let mut again = reloaded;
        assert!(again.remove(&uuid, "Notch").unwrap());
        assert!(!again.remove(&uuid, "Notch").unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn login_lists_check() {
        let dir = std::env::temp_dir().join(format!("icbm-login-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let uuid = Uuid::parse_str("069a79f444e94726a5befca90e38aaf5").unwrap();
        let mut lists = LoginLists::load(&dir);
        assert!(lists.check_login(&uuid, "Notch").is_ok());
        lists.banned.add(uuid, "Notch").unwrap();
        assert!(lists.check_login(&uuid, "Notch").is_err());
        lists.whitelist_enabled = true;
        // 被封禁后白名单检查也失败（先查封禁）
        assert!(lists.check_login(&uuid, "Notch").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
