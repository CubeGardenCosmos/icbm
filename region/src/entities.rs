//! 实体模拟：掉落物与 TNT（Phase 2 最小实体集）。
//!
//! 对照 vanilla 可观察行为：
//! - 掉落物（`minecraft:item`，`EntityType` 注册序 58）：方块破坏后生成，朝随机
//!   方向弹出、受重力下落、静止后可拾取；玩家拾取时播放 `entity.item.pickup`。
//! - TNT（`minecraft:tnt`，注册序 106）：点火后 `entity.tnt.primed`，4 秒后
//!   爆炸（`entity.generic.explode`），破坏范围内方块并生成 `CExplosion` 包。
//!
//! 实体 ID 由 region 从 `entity_base` 分配（core 在 `REGISTER_ACK` 下发），
//! 客户端经 `ADD_ENTITY`/`REMOVE_ENTITIES` 同步。本模块只做纯状态模拟，
//! 广播由 region 服务负责。

use icbm_mc_767::packets::play::block_state_ids;
use uuid::Uuid;

/// 实体类型注册表 ID（vanilla 1.21.1 `EntityType` 注册序，对照 master
/// `data/src/generated/entity_type.rs`）。
pub mod entity_type_ids {
    /// `minecraft:creeper`。
    pub const CREEPER: i32 = 23;
    /// `minecraft:item`（掉落物）。
    pub const ITEM: i32 = 58;
    /// `minecraft:tnt`。
    pub const TNT: i32 = 106;
}

/// 掉落物实体。
#[derive(Debug, Clone)]
pub struct ItemEntity {
    /// 实体 ID。
    pub entity_id: i32,
    /// UUID。
    pub uuid: Uuid,
    /// 位置（方块，整数坐标；客户端渲染用）。
    pub x: f64,
    /// 位置。
    pub y: f64,
    /// 位置。
    pub z: f64,
    /// 速度（方块/tick）。
    pub vx: f64,
    /// 速度。
    pub vy: f64,
    /// 速度。
    pub vz: f64,
    /// 物品名（`minecraft:` 前缀省略）。
    pub item: String,
    /// 数量。
    pub count: i32,
    /// 生存 tick（用于拾取延迟与消失）。
    pub age: u32,
    /// 可否被拾取（短时间内不可拾取，模拟 vanilla 拾取延迟）。
    pub pickable: bool,
}

/// TNT 实体。
#[derive(Debug, Clone)]
pub struct TntEntity {
    /// 实体 ID。
    pub entity_id: i32,
    /// UUID。
    pub uuid: Uuid,
    /// 位置（方块）。
    pub x: f64,
    /// 位置。
    pub y: f64,
    /// 位置。
    pub z: f64,
    /// 剩余引信 tick（vanilla 80 tick = 4 秒）。
    pub fuse: u16,
    /// 是否已点火（客户端显示点燃状态）。
    pub primed: bool,
}

/// 实体管理器（region 内所有非玩家实体）。
#[derive(Debug)]
pub struct EntityManager {
    /// 掉落物。
    pub items: Vec<ItemEntity>,
    /// TNT。
    pub tnts: Vec<TntEntity>,
    /// xorshift64 状态（确定性随机；固定种子可复现）。
    rng: u64,
}

impl Default for EntityManager {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            tnts: Vec::new(),
            rng: 0x5DEE_CE66_D9E3_779B, // 固定种子
        }
    }
}

impl EntityManager {
    /// 新建（空）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// xorshift64 随机数。
    #[allow(clippy::missing_const_for_fn)] // 修改 self.rng，不能 const
    fn next_random(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    /// [0,1) 均匀随机。
    fn rand_f64(&mut self) -> f64 {
        (self.next_random() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 实体总数。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len() + self.tnts.len()
    }

    /// 是否为空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty() && self.tnts.is_empty()
    }

    /// 生成掉落物（方块破坏/实体死亡时）。
    pub fn spawn_item(&mut self, entity_id: i32, x: f64, y: f64, z: f64, item: &str, count: i32) {
        // vanilla：掉落物随机弹出速度（约 ±0.2 方块/tick，水平小、竖直向上）
        let vx = (self.rand_f64() - 0.5) * 0.4;
        let vy = 0.2 + self.rand_f64() * 0.1;
        let vz = (self.rand_f64() - 0.5) * 0.4;
        self.items.push(ItemEntity {
            entity_id,
            uuid: Uuid::new_v4(),
            x,
            y,
            z,
            vx,
            vy,
            vz,
            item: item.to_owned(),
            count: count.clamp(1, 64),
            age: 0,
            pickable: false,
        });
    }

    /// 生成 TNT（点火）。
    pub fn spawn_tnt(&mut self, entity_id: i32, x: f64, y: f64, z: f64) {
        self.tnts.push(TntEntity {
            entity_id,
            uuid: Uuid::new_v4(),
            x,
            y,
            z,
            fuse: 80,
            primed: true,
        });
    }
}

/// 爆炸结果。
#[derive(Debug, Clone)]
pub struct ExplosionResult {
    /// 受影响方块（绝对坐标，按爆炸半径从中心向外）。
    pub affected_blocks: Vec<(i32, i32, i32)>,
}

/// 计算爆炸破坏的方块集（简化球体采样）。
///
/// 对照 vanilla `Explosion.collectBlocksAndDamage`：以中心为原点，按半径采样
/// 射线方向上的方块，受 `block_at` 阻挡（空气不阻挡）。此处简化：球体内全部
/// 非空气、非基岩方块被破坏。
#[must_use]
pub fn compute_explosion(
    center_x: f64,
    center_y: f64,
    center_z: f64,
    radius: f64,
    block_at: &mut impl FnMut(i32, i32, i32) -> i32,
) -> ExplosionResult {
    let mut affected = Vec::new();
    let r = radius.ceil() as i32;
    for dx in -r..=r {
        for dy in -r..=r {
            for dz in -r..=r {
                let x = center_x.floor() as i32 + dx;
                let y = center_y.floor() as i32 + dy;
                let z = center_z.floor() as i32 + dz;
                let dist = (dx as f64).powi(2) + (dy as f64).powi(2) + (dz as f64).powi(2);
                if dist > radius * radius {
                    continue;
                }
                let state = block_at(x, y, z);
                if state == 0 || state == block_state_ids::BEDROCK {
                    continue;
                }
                affected.push((x, y, z));
            }
        }
    }
    ExplosionResult {
        affected_blocks: affected,
    }
}

#[cfg(test)]
mod tests {
    use super::{EntityManager, ExplosionResult, compute_explosion, entity_type_ids};
    use icbm_mc_767::packets::play::block_state_ids;

    #[test]
    fn entity_type_ids_match_vanilla() {
        assert_eq!(entity_type_ids::CREEPER, 23);
        assert_eq!(entity_type_ids::ITEM, 58);
        assert_eq!(entity_type_ids::TNT, 106);
    }

    #[test]
    fn item_entity_spawn() {
        let mut manager = EntityManager::new();
        manager.spawn_item(100, 1.5, 64.0, 2.5, "stone", 3);
        assert_eq!(manager.len(), 1);
        let item = &manager.items[0];
        assert_eq!(item.item, "stone");
        assert_eq!(item.count, 3);
        assert!(!item.pickable);
    }

    #[test]
    fn tnt_spawn_with_fuse() {
        let mut manager = EntityManager::new();
        manager.spawn_tnt(200, 1.0, 65.0, 2.0);
        assert_eq!(manager.tnts.len(), 1);
        assert!(manager.tnts[0].primed);
        assert_eq!(manager.tnts[0].fuse, 80);
    }

    #[test]
    fn explosion_breaks_blocks_in_radius() {
        let mut world = std::collections::HashMap::new();
        // 中心方块是石头，周围基岩包围
        world.insert((0, 0, 0), block_state_ids::STONE);
        world.insert((1, 0, 0), block_state_ids::STONE);
        world.insert((0, 0, 1), block_state_ids::STONE);
        world.insert((-1, 0, 0), block_state_ids::BEDROCK);
        let get = |x: i32, y: i32, z: i32| *world.get(&(x, y, z)).unwrap_or(&0);
        let mut get_mut = get;
        let result: ExplosionResult = compute_explosion(0.0, 0.0, 0.0, 2.0, &mut get_mut);
        // 石头被破坏，基岩保留
        assert!(result.affected_blocks.contains(&(1, 0, 0)));
        assert!(!result.affected_blocks.contains(&(-1, 0, 0)));
    }
}
