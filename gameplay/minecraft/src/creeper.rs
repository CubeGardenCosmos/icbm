//! 敌对生物 Creeper：生成、追逐与引信 AI（vanilla `CreeperEntity` 行为）。
//!
//! 对照 vanilla `CreeperEntity`/`MobEntity` 的可观察行为：
//! - 夜晚（`doMobSpawning` 开启）按概率生成，上限 `MAX_CREEPERS`；
//! - 追踪最近玩家（16 格内），3 格内点燃引信（`FUSE_TIME` = 30 tick）后爆炸；
//! - 爆炸半径 3（`EXPLOSION_RADIUS`），由 region 调 `Explosion` 破坏方块。
//!
//! 本模块只做行为计算（输入世界时间/玩家位置/区域边界与 RNG → 输出动作
//! 事件），发包与方块破坏由 region 编排（`CreeperAction` 事件驱动）。

use uuid::Uuid;

/// `minecraft:creeper` 实体类型注册表 ID（vanilla 1.21.1 注册序）。
pub const ENTITY_TYPE_CREEPER: i32 = 23;

/// 每区域最多存活的 Creeper 数（vanilla 每区域怪物上限约 70，本实现取小）。
pub const MAX_CREEPERS: usize = 12;

/// 每 tick 生成 Creeper 的概率（夜晚约 2 秒一只；vanilla 每区块多次生成尝试，
/// 取较高值保证测试与演示可复现）。
pub const SPAWN_CHANCE: f64 = 1.0 / 40.0;

/// 敌对生物追踪/引爆半径（方块）：vanilla `CreeperEntity` 的
/// `CHARGED_CREEPER`/`FUSE_TIME` 与 `MobEntity` 目标半径。
pub const TARGET_RANGE: f64 = 16.0;

/// 点燃引信的玩家距离（vanilla 3 格内开始闪烁）。
pub const FUSE_RANGE: f64 = 3.0;

/// 引信时长（tick；vanilla `CreeperEntity.FUSE_TIME` = 30）。
pub const FUSE_TICKS: i32 = 30;

/// 爆炸半径（方块；vanilla `CreeperEntity.EXPLOSION_RADIUS`）。
pub const EXPLOSION_RADIUS: f64 = 3.0;

/// 夜晚时间窗（世界时间 % 24000；vanilla 13000..23000 为夜间，敌对生物生成）。
pub const NIGHT_START: i64 = 13_000;
pub const NIGHT_END: i64 = 23_000;

/// 区域内一只 Creeper。
#[derive(Debug, Clone)]
pub struct Creeper {
    /// 实体 ID（`entity_base + 生成序`）。
    pub entity_id: i32,
    /// 实体 UUID。
    pub uuid: Uuid,
    /// X（方块）。
    pub x: f64,
    /// Y（方块）。
    pub y: f64,
    /// Z（方块）。
    pub z: f64,
    /// 偏航角（度）。
    pub yaw: f32,
    /// 上次移动方向（1/4 方向索引；避免连续折返，寻路更自然）。
    pub last_dir: u8,
    /// 引信倒计时（tick；-1 = 未点燃）。点燃后闪烁并倒计时爆炸（vanilla 30 tick）。
    pub fuse: i32,
}

/// Creeper 单 tick 行为输出（region 据此发包/破坏方块）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CreeperAction {
    /// 移动（`delta_x`/`delta_z` × 4096）。
    Move(i32, i16, i16),
    /// 点燃引信（实体状态闪烁）。
    FuseStart(i32),
    /// 爆炸（`entity_id`, `x`, `y`, `z`；region 破坏方块并移除实体）。
    Explode(i32, f64, f64, f64),
}

/// 生成参数（由 region 提供随机数，保持确定性）。
pub struct SpawnParams<'a> {
    /// 区域 X（区块坐标，右移 5 位）。
    pub rx: i32,
    /// 区域 Z。
    pub rz: i32,
    /// 出生高度（superflat 地表站立 Y）。
    pub spawn_y: f64,
    /// 实体 ID 基址。
    pub entity_base: i32,
    /// 随机数源（region 的确定性 RNG）。
    pub rand_f64: &'a mut dyn FnMut() -> f64,
}

impl Creeper {
    /// 新建（未点燃引信）。
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // Uuid::new_v4 非常量，但 new 本身可 const
    pub fn new(entity_id: i32, uuid: Uuid, x: f64, y: f64, z: f64, yaw: f32) -> Self {
        Self {
            entity_id,
            uuid,
            x,
            y,
            z,
            yaw,
            last_dir: 4, // 初始无方向
            fuse: -1,    // 未点燃
        }
    }

    /// 单 tick 行为（vanilla `CreeperEntity.tick`）：引信倒计时；未点燃时
    /// 追踪最近玩家（`nearest` 为玩家位置，None 表示无玩家），3 格内点燃、
    /// 16 格内朝玩家移动。
    ///
    /// 返回动作事件（region 据此发包/爆炸）；`None` 表示无动作。
    pub fn tick(
        &mut self,
        nearest: Option<(f64, f64, f64)>,
        min_x: f64,
        max_x: f64,
        min_z: f64,
        max_z: f64,
        rand_f64: &mut impl FnMut() -> f64,
    ) -> Option<CreeperAction> {
        if self.fuse >= 0 {
            // 引信倒计时
            self.fuse -= 1;
            if self.fuse <= 0 {
                return Some(CreeperAction::Explode(
                    self.entity_id,
                    self.x,
                    self.y,
                    self.z,
                ));
            }
            return None;
        }
        let (px, py, pz) = nearest?;
        let dx = px - self.x;
        let dz = pz - self.z;
        let horizontal = dx.hypot(dz);
        if horizontal <= FUSE_RANGE {
            self.fuse = FUSE_TICKS;
            return Some(CreeperAction::FuseStart(self.entity_id));
        }
        if horizontal <= TARGET_RANGE && (py - self.y).abs() < 8.0 && rand_f64() < 0.2 {
            // 朝玩家移动 1 格（vanilla 步行速度约 0.25 格/tick）
            let step = if dx.abs() > dz.abs() {
                (dx.signum(), 0.0)
            } else {
                (0.0, dz.signum())
            };
            let next_x = self.x + step.0;
            let next_z = self.z + step.1;
            if (min_x..max_x).contains(&next_x) && (min_z..max_z).contains(&next_z) {
                self.x = next_x;
                self.z = next_z;
                return Some(CreeperAction::Move(
                    self.entity_id,
                    (step.0 * 4096.0) as i16,
                    (step.1 * 4096.0) as i16,
                ));
            }
        }
        None
    }

    /// 在区域边界内随机生成一只（`entity_base + 序` 分配实体 ID）。
    #[must_use]
    pub fn spawn(params: &mut SpawnParams<'_>, index: usize) -> Self {
        let entity_id = params.entity_base + i32::try_from(index).unwrap_or(i32::MAX);
        let cx = params.rx * 32 + ((params.rand_f64)() * 32.0) as i32;
        let cz = params.rz * 32 + ((params.rand_f64)() * 32.0) as i32;
        Self::new(
            entity_id,
            Uuid::new_v4(),
            f64::from(cx * 16) + (params.rand_f64)() * 16.0,
            params.spawn_y,
            f64::from(cz * 16) + (params.rand_f64)() * 16.0,
            ((params.rand_f64)() * 360.0) as f32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Creeper, CreeperAction, FUSE_RANGE, FUSE_TICKS, TARGET_RANGE};

    fn no_rand() -> impl FnMut() -> f64 {
        || 0.5
    }

    #[test]
    fn fuse_countdown_explodes() {
        let mut creeper = Creeper::new(1, uuid::Uuid::new_v4(), 0.0, 0.0, 0.0, 0.0);
        creeper.fuse = 1;
        let action = creeper.tick(None, 0.0, 100.0, 0.0, 100.0, &mut no_rand());
        assert_eq!(action, Some(CreeperAction::Explode(1, 0.0, 0.0, 0.0)));
    }

    #[test]
    fn fuses_within_range() {
        let mut creeper = Creeper::new(1, uuid::Uuid::new_v4(), 0.0, 0.0, 0.0, 0.0);
        let action = creeper.tick(
            Some((1.5, 0.0, 0.0)),
            0.0,
            100.0,
            0.0,
            100.0,
            &mut no_rand(),
        );
        assert_eq!(action, Some(CreeperAction::FuseStart(1)));
        assert_eq!(creeper.fuse, FUSE_TICKS);
    }

    #[test]
    fn chases_player_within_target_range() {
        let mut creeper = Creeper::new(1, uuid::Uuid::new_v4(), 10.0, 0.0, 10.0, 0.0);
        let action = creeper.tick(
            Some((20.0, 0.0, 10.0)),
            0.0,
            100.0,
            0.0,
            100.0,
            &mut || 0.1, // < 0.2 触发移动
        );
        match action {
            Some(CreeperAction::Move(id, dx, _dz)) => {
                assert_eq!(id, 1);
                assert!(dx > 0, "moves toward +x");
            }
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn no_chase_outside_target_range() {
        let mut creeper = Creeper::new(1, uuid::Uuid::new_v4(), 0.0, 0.0, 0.0, 0.0);
        let action = creeper.tick(
            Some((TARGET_RANGE + 5.0, 0.0, 0.0)),
            0.0,
            100.0,
            0.0,
            100.0,
            &mut no_rand(),
        );
        assert_eq!(action, None);
    }

    #[test]
    fn spawn_within_region_bounds() {
        let mut rng = || 0.25;
        let mut params = super::SpawnParams {
            rx: 1,
            rz: -1,
            spawn_y: -60.0,
            entity_base: 1000,
            rand_f64: &mut rng,
        };
        let creeper = Creeper::spawn(&mut params, 0);
        assert_eq!(creeper.entity_id, 1000);
        assert!(creeper.x >= 512.0 && creeper.x < 1024.0, "x={}", creeper.x);
        assert!(creeper.z >= -512.0 && creeper.z < 0.0, "z={}", creeper.z);
        assert_eq!(creeper.y, -60.0);
        assert_eq!(creeper.fuse, -1);
        // FUSE_RANGE 已导出供 region 使用
        assert!(FUSE_RANGE > 0.0);
    }
}
