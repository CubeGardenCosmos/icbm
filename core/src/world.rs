//! 服务器共享世界状态。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use icbm_gameplay_minecraft::gamerules::GameRuleStore;
use icbm_mc_767::packets::play::BlockPos;
use uuid::Uuid;

use crate::client::Client;
use crate::region_registry::RegionRegistry;

/// 玩家等待中的区块请求（全局串行，Phase 1）。
#[derive(Debug, Clone)]
pub struct ChunkPending {
    /// 请求玩家。
    pub player: Uuid,
    /// 预期区块总数（`(2r+1)^2`）。
    pub total: usize,
    /// 已收到数。
    pub received: usize,
    /// 是否已发批次开始。
    pub in_batch: bool,
}

/// 共享世界状态（各线程经锁访问）。
pub struct WorldState {
    /// 在线玩家（UUID → 客户端）。
    pub players: HashMap<Uuid, Arc<Client>>,
    /// 游戏规则。
    pub gamerules: GameRuleStore,
    /// region 注册表。
    pub regions: RegionRegistry,
    /// 全局串行的区块请求。
    pub chunk_pending: Option<ChunkPending>,
    /// 世界目录。
    pub world_dir: PathBuf,
    /// 世界出生点。
    pub spawn: BlockPos,
}

impl WorldState {
    /// 新建世界状态。
    #[must_use]
    pub fn new(world_dir: PathBuf) -> Self {
        Self {
            players: HashMap::new(),
            gamerules: GameRuleStore::new(),
            regions: RegionRegistry::new(),
            chunk_pending: None,
            world_dir,
            spawn: BlockPos::new(0, 4, 0),
        }
    }
}
