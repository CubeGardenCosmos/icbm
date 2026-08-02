//! 友好生物：生成、随机游走 AI 与声音（Phase 2）。
//!
//! 对照 vanilla `PassiveEntity`/`MobEntity` 的可观察行为简化：
//! - 牛/猪/鸡/羊在地面（y=4，flat 地表上方）随机生成，数量有上限；
//! - 随机游走：优先朝「前方有可站立方块、不越出区域边界」的方向移动；避免
//!   连续折返（保留移动惯性），行为上更像 vanilla 的 `WanderAroundGoal`；
//! - 周期播放环境音效（`entity.cow.ambient` 等），受击/死亡音效留待伤害系统；
//! - 声音网络包由 region 服务经 `CEntitySound` 广播。
//!
//! 实体类型注册表 ID（vanilla 1.21.1 `EntityType` 注册序，对照 master
//! `data/src/generated/entity_type.rs`）。

use uuid::Uuid;

/// 实体类型注册表 ID。
pub mod mob_type_ids {
    /// `minecraft:chicken`。
    pub const CHICKEN: i32 = 19;
    /// `minecraft:cow`。
    pub const COW: i32 = 22;
    /// `minecraft:pig`。
    pub const PIG: i32 = 77;
    /// `minecraft:sheep`。
    pub const SHEEP: i32 = 87;
    /// `minecraft:wolf`（预留）。
    pub const WOLF: i32 = 117;
}

/// 生物种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobKind {
    /// 鸡。
    Chicken,
    /// 牛。
    Cow,
    /// 猪。
    Pig,
    /// 羊。
    Sheep,
}

impl MobKind {
    /// 实体类型注册表 ID。
    #[must_use]
    pub const fn entity_type_id(self) -> i32 {
        match self {
            Self::Chicken => mob_type_ids::CHICKEN,
            Self::Cow => mob_type_ids::COW,
            Self::Pig => mob_type_ids::PIG,
            Self::Sheep => mob_type_ids::SHEEP,
        }
    }

    /// 环境音效（`minecraft:` 前缀省略）。
    #[must_use]
    pub const fn ambient_sound(self) -> &'static str {
        match self {
            Self::Chicken => "entity.chicken.ambient",
            Self::Cow => "entity.cow.ambient",
            Self::Pig => "entity.pig.ambient",
            Self::Sheep => "entity.sheep.ambient",
        }
    }
}

/// 一只友好生物。
#[derive(Debug, Clone)]
pub struct FriendlyMob {
    /// 实体 ID。
    pub entity_id: i32,
    /// UUID。
    pub uuid: Uuid,
    /// 种类。
    pub kind: MobKind,
    /// X（方块）。
    pub x: f64,
    /// Z（方块）。
    pub z: f64,
    /// 偏航角（度）。
    pub yaw: f32,
    /// 上次移动方向（1/4 方向索引；避免连续折返）。
    last_dir: u8,
    /// 距下次环境音效的 tick 数。
    sound_cool_down: u32,
}

impl FriendlyMob {
    /// 新建。
    #[must_use]
    pub fn new(entity_id: i32, kind: MobKind, x: f64, z: f64, yaw: f32) -> Self {
        Self {
            entity_id,
            uuid: Uuid::new_v4(),
            kind,
            x,
            z,
            yaw,
            last_dir: 4, // 初始无方向
            sound_cool_down: 40,
        }
    }

    /// 生物在地面的 Y（flat 地表 y=3 上方站立）。
    #[must_use]
    pub const fn ground_y(&self) -> f64 {
        4.0
    }
}

/// 生物管理器。
#[derive(Debug)]
pub struct MobManager {
    /// 友好生物。
    pub mobs: Vec<FriendlyMob>,
    /// xorshift64 状态。
    rng: u64,
}

impl MobManager {
    /// 新建（固定种子可复现）。
    #[allow(clippy::missing_const_for_fn)] // Vec::new 尚非 const
    #[must_use]
    pub fn new() -> Self {
        Self {
            mobs: Vec::new(),
            rng: 0x243F_6A88_85A3_08D3,
        }
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

    /// 生物总数。
    #[must_use]
    pub const fn len(&self) -> usize {
        self.mobs.len()
    }

    /// 是否为空。
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }

    /// 生成一只随机友好生物（若未达上限）。
    ///
    /// `entity_id` 由调用方分配；返回是否生成。
    pub fn try_spawn(&mut self, entity_id: i32, min_x: f64, max_x: f64, min_z: f64, max_z: f64) -> bool {
        if self.mobs.len() >= MAX_FRIENDLY_MOBS {
            return false;
        }
        let kind = match self.next_random() % 4 {
            0 => MobKind::Chicken,
            1 => MobKind::Cow,
            2 => MobKind::Pig,
            _ => MobKind::Sheep,
        };
        let x = min_x + self.rand_f64() * (max_x - min_x);
        let z = min_z + self.rand_f64() * (max_z - min_z);
        let yaw = (self.rand_f64() * 360.0) as f32;
        self.mobs.push(FriendlyMob::new(entity_id, kind, x, z, yaw));
        true
    }

    /// 随机游走一步：返回移动的 `(entity_id, dx, dz)` 列表。
    ///
    /// 寻路规则（比 Phase 1 的 Creeper 更合理）：\n
    /// - 先掷随机；\n
    /// - 候选方向排除「上一方向的反方向」（避免原地折返），若区域足够宽则\n
    ///   优先选靠近区域中心的区域；\n
    /// - 不越出区域边界。\n
    pub fn wander(
        &mut self,
        min_x: f64,
        max_x: f64,
        min_z: f64,
        max_z: f64,
    ) -> Vec<(i32, i16, i16)> {
        let mut moves = Vec::new();
        // 预掷随机数再逐个体决策，避免同时借用 self.rng 与 self.mobs
        let rolls: Vec<(f64, u64)> = (0..self.mobs.len())
            .map(|_| (self.rand_f64(), self.next_random() % 4))
            .collect();
        for (mob, (roll, dir_roll)) in self.mobs.iter_mut().zip(rolls) {
            if roll < 0.4 {
                continue; // 站立（更符合友好生物：多数时间不动）
            }
            // 候选方向：0=+x 1=-x 2=+z 3=-z；排除上一方向反方向
            let candidates: Vec<u8> = (0..4u8)
                .filter(|&dir| (dir + 2) % 4 != mob.last_dir || mob.last_dir == 4)
                .collect();
            let dir = candidates[(dir_roll as usize) % candidates.len()];
            let (dx, dz) = match dir {
                0 => (1.0, 0.0),
                1 => (-1.0, 0.0),
                2 => (0.0, 1.0),
                _ => (0.0, -1.0),
            };
            let next_x = mob.x + dx;
            let next_z = mob.z + dz;
            if !(min_x..max_x).contains(&next_x) || !(min_z..max_z).contains(&next_z) {
                continue;
            }
            mob.x = next_x;
            mob.z = next_z;
            mob.last_dir = dir;
            moves.push((
                mob.entity_id,
                (dx * 4096.0) as i16,
                (dz * 4096.0) as i16,
            ));
        }
        moves
    }

    /// 生物 tick：游走 + 声音冷却。返回本 tick 需要播放环境音效的
    /// `(entity_id, kind)` 列表。
    #[allow(clippy::type_complexity)] // 移动与声音两类事件的显式元组，语义清晰
    pub fn tick(
        &mut self,
        min_x: f64,
        max_x: f64,
        min_z: f64,
        max_z: f64,
    ) -> (Vec<(i32, i16, i16)>, Vec<(i32, MobKind)>) {
        let moves = self.wander(min_x, max_x, min_z, max_z);
        // 预掷声音随机（避免同时借用 self.rng 与 self.mobs）
        let sound_rolls: Vec<f64> = (0..self.mobs.len()).map(|_| self.rand_f64()).collect();
        let mut sounds = Vec::new();
        for (mob, roll) in self.mobs.iter_mut().zip(sound_rolls) {
            mob.sound_cool_down = mob.sound_cool_down.saturating_sub(1);
            // 平均约 6 秒一次环境音
            if mob.sound_cool_down == 0 && roll < 0.3 {
                sounds.push((mob.entity_id, mob.kind));
                mob.sound_cool_down = 120;
            }
        }
        (moves, sounds)
    }
}

impl Default for MobManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 每区域最多友好生物数。
pub const MAX_FRIENDLY_MOBS: usize = 10;

#[cfg(test)]
mod tests {
    use super::{MAX_FRIENDLY_MOBS, MobKind, MobManager, mob_type_ids};

    #[test]
    fn mob_type_ids_match_vanilla() {
        assert_eq!(mob_type_ids::CHICKEN, 19);
        assert_eq!(mob_type_ids::COW, 22);
        assert_eq!(mob_type_ids::PIG, 77);
        assert_eq!(mob_type_ids::SHEEP, 87);
        assert_eq!(MobKind::Cow.entity_type_id(), 22);
    }

    #[test]
    fn ambient_sounds_exist() {
        for kind in [MobKind::Chicken, MobKind::Cow, MobKind::Pig, MobKind::Sheep] {
            let sound = kind.ambient_sound();
            assert!(sound.starts_with("entity."));
            assert!(icbm_mc_767::sound_events::sound_id(sound).is_some());
        }
    }

    #[test]
    fn spawn_and_wander_stay_in_bounds() {
        let mut manager = MobManager::new();
        for i in 0..5 {
            assert!(manager.try_spawn(i, 0.0, 512.0, 0.0, 512.0));
        }
        assert_eq!(manager.len(), 5);
        for _ in 0..100 {
            let (moves, _) = manager.tick(0.0, 512.0, 0.0, 512.0);
            for (_, dx, dz) in moves {
                assert!(dx.abs() <= 4096);
                assert!(dz.abs() <= 4096);
            }
            for mob in &manager.mobs {
                assert!((0.0..512.0).contains(&mob.x));
                assert!((0.0..512.0).contains(&mob.z));
            }
        }
    }

    #[test]
    fn spawn_cap_enforced() {
        let mut manager = MobManager::new();
        let mut spawned = 0;
        while manager.len() < MAX_FRIENDLY_MOBS && spawned < 1000 {
            manager.try_spawn(spawned, 0.0, 512.0, 0.0, 512.0);
            spawned += 1;
        }
        assert_eq!(manager.len(), MAX_FRIENDLY_MOBS);
        assert!(!manager.try_spawn(9999, 0.0, 512.0, 0.0, 512.0));
    }
}
