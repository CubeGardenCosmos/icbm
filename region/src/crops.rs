//! 农作物生长系统（对照 vanilla 随机 tick 生长简化模型）。
//!
//! 每个已加载区块每 tick 按 `randomTickSpeed`（默认 3）次随机抽查：取随机
//! 列的地表顶方块，命中作物（小麦/胡萝卜/马铃薯/甜菜根/南瓜茎/西瓜茎/可可）
//! 且未成熟时以 1/3 概率推进 `age`（对照 `CropBlock.randomTick` 的
//! `getAvailableMoisture` 加权，此处简化统一 1/3；可可 2/3）。

use icbm_mc_767::packets::play::block_state_ids as ids;

/// 一种作物：`(age=0 状态 ID, 最大 age)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropKind {
    /// age=0 的全局状态 ID。
    pub base: i32,
    /// 最大 age（成熟态）。
    pub max_age: i32,
}

/// 全部可生长作物（vanilla 全局状态 ID，对照 master 生成数据：
/// `wheat` 4278 / `carrots` 8595 / `potatoes` 8603 / `beetroots` 12509 /
/// `pumpkin_stem` 6821 / `melon_stem` 6829 / `cocoa` 7419）。
pub const CROPS: [CropKind; 7] = [
    CropKind { base: ids::WHEAT_AGE_0, max_age: 7 },
    CropKind { base: ids::CARROTS_AGE_0, max_age: 7 },
    CropKind { base: ids::POTATOES_AGE_0, max_age: 7 },
    CropKind { base: ids::BEETROOTS_AGE_0, max_age: 3 },
    CropKind { base: ids::PUMPKIN_STEM_AGE_0, max_age: 7 },
    CropKind { base: ids::MELON_STEM_AGE_0, max_age: 7 },
    CropKind { base: ids::COCOA_AGE_0, max_age: 2 },
];

/// 状态 ID 对应的作物（非作物返回 `None`）。
#[must_use]
pub const fn is_crop(state_id: i32) -> Option<CropKind> {
    let mut i = 0;
    while i < CROPS.len() {
        let crop = CROPS[i];
        let age = state_id - crop.base;
        if age >= 0 && age <= crop.max_age {
            return Some(crop);
        }
        i += 1;
    }
    None
}

/// 该作物在给定 age 的状态 ID。
#[must_use]
pub const fn crop_state(crop: CropKind, age: i32) -> i32 {
    crop.base + age
}

#[cfg(test)]
mod tests {
    use super::{CROPS, crop_state, is_crop};
    use icbm_mc_767::packets::play::block_state_ids as ids;

    #[test]
    fn crop_ids_match_vanilla() {
        assert_eq!(ids::WHEAT_AGE_0, 4278);
        assert_eq!(ids::CARROTS_AGE_0, 8595);
        assert_eq!(ids::POTATOES_AGE_0, 8603);
        assert_eq!(ids::BEETROOTS_AGE_0, 12509);
        assert_eq!(ids::PUMPKIN_STEM_AGE_0, 6821);
        assert_eq!(ids::MELON_STEM_AGE_0, 6829);
        assert_eq!(ids::COCOA_AGE_0, 7419);
    }

    #[test]
    fn crop_ranges() {
        for crop in CROPS {
            // age 0..max 都属于该作物
            for age in 0..=crop.max_age {
                let state = crop_state(crop, age);
                assert_eq!(is_crop(state), Some(crop), "state {state}");
            }
            // 成熟态 + 1 不再是该作物
            assert!(
                is_crop(crop_state(crop, crop.max_age + 1)).is_none()
                    || is_crop(crop_state(crop, crop.max_age + 1))
                        != Some(crop)
            );
        }
        // 非作物
        assert!(is_crop(ids::STONE).is_none());
        assert!(is_crop(ids::AIR).is_none());
        assert!(is_crop(ids::GRASS_BLOCK).is_none());
    }

    #[test]
    fn crops_do_not_overlap() {
        for (i, a) in CROPS.iter().enumerate() {
            for b in &CROPS[i + 1..] {
                let a_max = crop_state(*a, a.max_age);
                let b_max = crop_state(*b, b.max_age);
                assert!(
                    a.base > b_max || b.base > a_max,
                    "crops {a:?} and {b:?} overlap"
                );
            }
        }
    }
}
