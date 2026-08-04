//! 流体模拟（水、岩浆）：vanilla `FlowingFluid`/`LiquidBlock` 可观察行为的分块扫描模型。
//!
//! 方块 LEVEL 属性 0..15（`LiquidBlock.LEVEL`）语义与 vanilla 一致：
//! - `0` = 源块（amount 8，永不消失）；
//! - `1..7` = 流动态，amount = `8 - LEVEL`（离源越远越浅，LEVEL 7 = amount 1）；
//! - `8..15` = falling 态（下落中），amount = `16 - LEVEL`（8 = amount 8 下落）。
//!
//! 每 tick 对含流体的脏区块扫描，对每个流体方块执行 vanilla `FlowingFluid.tick`
//! 两步：非源块先 `getNewLiquid` 更新自身（上方有流体 → falling；水平邻居最高
//! amount - dropoff，≤ 0 → 置空气消散），再 `spread`（下方可替换 → 下落为
//! falling；否则水平扩散：amount - dropoff，falling 以 amount 7 扩散）。
//! `dropoff`：水 1（平地流 7 格）、岩浆 2（平地流 3 格），对照
//! `WaterFluid.getDropOff`/`LavaFluid.getDropOff`。
//!
//! 与 vanilla 的差异（有意简化，记录于 parity matrix）：
//! - 不实现 slope-distance 寻路与计划 tick（`getTickDelay` 水 5 / 岩浆 30），
//!   每 tick 扫描全部流体脏区块，收敛到与 vanilla 相同的稳定形态；
//! - 无无限水源（vanilla `waterSourceConversion` 默认 false，行为一致）；
//! - 可替换方块集简化为空气/异种流体（flat 世界无植物等可替换方块）。
//!
//! 本模块只做纯计算（输入当前列快照 → 输出待更新的方块与状态），不持有世界
//! 状态；region 每 tick 对已加载区块调用 [`tick_fluid`]。

use icbm_mc_767::packets::play::block_state_ids;

/// 流体类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FluidKind {
    /// 水。
    Water,
    /// 岩浆。
    Lava,
}

/// 状态 ID 是否为流体（水或岩浆）。
#[must_use]
pub const fn is_fluid(state_id: i32) -> bool {
    FluidKind::from_state_id(state_id).is_some()
}

impl FluidKind {
    /// 状态 ID → 流体类型（非流体返回 `None`）。
    #[must_use]
    pub const fn from_state_id(state_id: i32) -> Option<Self> {
        if state_id >= block_state_ids::WATER && state_id < block_state_ids::WATER + 16 {
            Some(Self::Water)
        } else if state_id >= block_state_ids::LAVA && state_id < block_state_ids::LAVA + 16 {
            Some(Self::Lava)
        } else {
            None
        }
    }

    /// 状态 ID → 方块 LEVEL 属性（0 源块 … 15 最深 falling）。
    #[must_use]
    pub const fn level_from_state_id(state_id: i32) -> Option<i32> {
        match Self::from_state_id(state_id) {
            Some(Self::Water) => Some(state_id - block_state_ids::WATER),
            Some(Self::Lava) => Some(state_id - block_state_ids::LAVA),
            None => None,
        }
    }

    /// LEVEL → 状态 ID。
    #[must_use]
    pub const fn state_id(self, level: i32) -> i32 {
        let base = match self {
            Self::Water => block_state_ids::WATER,
            Self::Lava => block_state_ids::LAVA,
        };
        let level = if level < 0 {
            0
        } else if level > 15 {
            15
        } else {
            level
        };
        base + level
    }

    /// 计划 tick 延迟（vanilla `getTickDelay`：水 5、岩浆 30）。
    #[must_use]
    pub const fn tick_delay(self) -> u64 {
        match self {
            Self::Water => 5,
            Self::Lava => 30,
        }
    }

    /// 每格流动衰减（vanilla `getDropOff`：水 1 / 岩浆 2）。
    #[must_use]
    const fn dropoff(self) -> i32 {
        match self {
            Self::Water => 1,
            Self::Lava => 2,
        }
    }

    /// LEVEL → amount（vanilla `FluidState.getAmount`：源 8、流动 8-L、falling 16-L）。
    #[must_use]
    const fn amount_of(level: i32) -> i32 {
        if level == 0 {
            8
        } else if level < 8 {
            8 - level
        } else {
            16 - level
        }
    }

    /// amount → 方块 LEVEL（`getLegacyLevel` 反向；falling 用 8..15 段）。
    #[must_use]
    const fn level_of(amount: i32) -> i32 {
        let amount = if amount < 1 {
            1
        } else if amount > 8 {
            8
        } else {
            amount
        };
        8 - amount
    }

    /// 是否源块（LEVEL 0）。
    #[must_use]
    const fn is_source(level: i32) -> bool {
        level == 0
    }

    /// 是否 falling 态（LEVEL 8..15）。
    #[must_use]
    const fn is_falling(level: i32) -> bool {
        level >= 8
    }
}

/// 流体 tick 产生的方块更新。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidUpdate {
    /// X（世界坐标）。
    pub x: i32,
    /// Y。
    pub y: i32,
    /// Z。
    pub z: i32,
    /// 新状态 ID（`0` = 空气，表示消散）。
    pub state_id: i32,
}

/// 单位置流体 tick 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidTickResult {
    /// 本次 tick 产生的方块更新。
    pub updates: [FluidUpdate; 6],
    /// 实际产生的更新数（`updates` 前 `len` 项有效）。
    pub len: usize,
    /// 非源块状态变化后需重新调度自身（vanilla `FlowingFluid.tick`：
    /// `getNewLiquid` 变化时 `scheduleTick(self, getSpreadDelay)`）。
    pub reschedule_self: bool,
}

/// 单方块 `FlowingFluid.tick`（vanilla）：getNewLiquid 更新自身 + spread 扩散。
///
/// 由计划 tick 引擎驱动（每个流体方块在放置/邻居变化时被调度）；与
/// [`tick_fluid`] 的区域扫描模型并存，供计划 tick 路径使用。
///
/// `block_at(x, y, z)` 返回当前状态 ID（`0` = 空气）。越界视为空气。
#[must_use]
pub fn tick_fluid_at(
    x: i32,
    y: i32,
    z: i32,
    mut block_at: impl FnMut(i32, i32, i32) -> i32,
) -> FluidTickResult {
    let directions = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    let state = block_at(x, y, z);
    let Some(kind) = FluidKind::from_state_id(state) else {
        return FluidTickResult {
            updates: [FluidUpdate {
                x,
                y,
                z,
                state_id: 0,
            }; 6],
            len: 0,
            reschedule_self: false,
        };
    };
    let level = FluidKind::level_from_state_id(state).expect("fluid just matched");
    let amount = FluidKind::amount_of(level);
    let is_source = FluidKind::is_source(level);
    let dropoff = kind.dropoff();

    let mut updates: [FluidUpdate; 6] = [FluidUpdate {
        x,
        y,
        z,
        state_id: 0,
    }; 6];
    let mut n = 0;
    let mut reschedule_self = false;

    // 1. 非源块先更新自身（vanilla `FlowingFluid.tick` 的 getNewLiquid 段）
    let mut current = level;
    let mut current_amount = amount;
    if !is_source {
        let new_level = new_self_level(x, y, z, kind, dropoff, &mut block_at, directions);
        let Some(new_level) = new_level else {
            // 消散（vanilla：置空气），不再扩散
            updates[n] = FluidUpdate {
                x,
                y,
                z,
                state_id: 0,
            };
            return FluidTickResult {
                updates,
                len: n + 1,
                reschedule_self: false,
            };
        };
        if new_level != current {
            current = new_level;
            current_amount = FluidKind::amount_of(current);
            reschedule_self = true; // 非源块变化 → 重新调度自身
            updates[n] = FluidUpdate {
                x,
                y,
                z,
                state_id: kind.state_id(current),
            };
            n += 1;
        }
    }

    // 2. 扩散（vanilla `FlowingFluid.spread`）
    let below = block_at(x, y - 1, z);
    let below_kind = FluidKind::from_state_id(below);
    let below_spreadable = below == 0 || (below_kind.is_some() && below_kind != Some(kind));
    if below_spreadable {
        updates[n] = FluidUpdate {
            x,
            y: y - 1,
            z,
            state_id: kind.state_id(8), // falling
        };
        n += 1;
        // 源块 + 至少 3 个源邻居：下落的同时也向侧边扩散
        if is_source && source_neighbor_count_at(&mut block_at, x, y, z, kind, directions) >= 3 {
            n += spread_to_sides_at(
                &mut block_at,
                &mut updates[n..],
                x,
                y,
                z,
                kind,
                current_amount,
                dropoff,
                false,
                directions,
            );
        }
    } else if below_kind != Some(kind) {
        // 下方是实体方块：源块/流动态/falling 一律向侧边扩散
        n += spread_to_sides_at(
            &mut block_at,
            &mut updates[n..],
            x,
            y,
            z,
            kind,
            current_amount,
            dropoff,
            FluidKind::is_falling(current),
            directions,
        );
    }

    FluidTickResult {
        updates,
        len: n,
        reschedule_self,
    }
}

/// `getNewLiquid` 的自身 LEVEL 计算（vanilla `FlowingFluid.getNewLiquid`）：
/// 上方同流体 → falling（8）；否则水平邻居最高 amount - dropoff，≤ 0 → 消散
/// （返回 `None`）。
fn new_self_level(
    x: i32,
    y: i32,
    z: i32,
    kind: FluidKind,
    dropoff: i32,
    block_at: &mut impl FnMut(i32, i32, i32) -> i32,
    directions: [(i32, i32); 4],
) -> Option<i32> {
    if FluidKind::from_state_id(block_at(x, y + 1, z)) == Some(kind) {
        return Some(8); // falling：上方有流体
    }
    let mut highest = 0;
    for (dx, dz) in directions {
        let ns = block_at(x + dx, y, z + dz);
        if FluidKind::from_state_id(ns) == Some(kind) {
            let nl = FluidKind::level_from_state_id(ns).expect("fluid just matched");
            highest = highest.max(FluidKind::amount_of(nl));
        }
    }
    let k = highest - dropoff;
    if k <= 0 {
        return None; // 消散（vanilla：置空气）
    }
    Some(FluidKind::level_of(k))
}

/// 水平方向同流体源块（LEVEL 0）数量（vanilla `sourceNeighborCount`；单位置版）。
fn source_neighbor_count_at(
    block_at: &mut impl FnMut(i32, i32, i32) -> i32,
    x: i32,
    y: i32,
    z: i32,
    kind: FluidKind,
    directions: [(i32, i32); 4],
) -> i32 {
    let mut count = 0;
    for (dx, dz) in directions {
        let ns = block_at(x + dx, y, z + dz);
        if FluidKind::from_state_id(ns) == Some(kind)
            && FluidKind::level_from_state_id(ns) == Some(0)
        {
            count += 1;
        }
    }
    count
}

/// 水平扩散（vanilla `spreadToSides`；单位置版，写入 `updates` 并返回写入数）。
#[allow(clippy::too_many_arguments)] // 扩散参数多但语义单一
fn spread_to_sides_at(
    block_at: &mut impl FnMut(i32, i32, i32) -> i32,
    updates: &mut [FluidUpdate],
    x: i32,
    y: i32,
    z: i32,
    kind: FluidKind,
    amount: i32,
    dropoff: i32,
    falling: bool,
    directions: [(i32, i32); 4],
) -> usize {
    let side_amount = if falling { 7 } else { amount - dropoff };
    if side_amount <= 0 {
        return 0; // 边缘（amount 1 / 岩浆 amount ≤ 2）不再扩散
    }
    let side_level = FluidKind::level_of(side_amount);
    let side_state = kind.state_id(side_level);
    let mut n = 0;
    for (dx, dz) in directions {
        let ns = block_at(x + dx, y, z + dz);
        let neighbor_kind = FluidKind::from_state_id(ns);
        if ns == 0 || (neighbor_kind.is_some() && neighbor_kind != Some(kind)) {
            updates[n] = FluidUpdate {
                x: x + dx,
                y,
                z: z + dz,
                state_id: side_state,
            };
            n += 1;
        }
    }
    n
}

/// 流体扩散：遍历指定区域内的流体方块，生成更新。
///
/// `block_at(x, y, z)` 返回当前状态 ID（`0` = 空气）；`set` 收集更新，由调用方
/// 在扫描结束后应用（同一 tick 内各流体方块看到的是上一 tick 的快照，顺序无关）。
/// 区域 y 范围即世界可建造高度（-64..319），扫描按 y 从高到低（先处理高处
/// 源块/落柱，行为与 vanilla 逐方块 tick 收敛一致）。
#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // 区域边界 + 两个回调，语义清晰
pub fn tick_fluid(
    min_x: i32,
    min_y: i32,
    min_z: i32,
    max_x: i32,
    max_y: i32,
    max_z: i32,
    mut block_at: impl FnMut(i32, i32, i32) -> i32,
    mut apply: impl FnMut(FluidUpdate),
) {
    let directions = [(1, 0), (-1, 0), (0, 1), (0, -1)];
    for y in (min_y..=max_y).rev() {
        for x in min_x..=max_x {
            for z in min_z..=max_z {
                let state = block_at(x, y, z);
                let Some(kind) = FluidKind::from_state_id(state) else {
                    continue;
                };
                let level = FluidKind::level_from_state_id(state).expect("fluid just matched");
                let amount = FluidKind::amount_of(level);
                let is_source = FluidKind::is_source(level);
                let dropoff = kind.dropoff();

                // 1. 非源块先更新自身（vanilla `FlowingFluid.tick` 的
                //    `getNewLiquid` 段）：上方同流体 → falling（LEVEL 8）；
                //    否则水平邻居最高 amount - dropoff，≤ 0 → 消散为空气。
                let mut current = level;
                let mut current_amount = amount;
                if !is_source {
                    let above = block_at(x, y + 1, z);
                    let new_level = if FluidKind::from_state_id(above) == Some(kind) {
                        8 // falling：上方有流体
                    } else {
                        let mut highest = 0;
                        for (dx, dz) in directions {
                            let nx = x + dx;
                            let nz = z + dz;
                            if nx < min_x || nx > max_x || nz < min_z || nz > max_z {
                                continue;
                            }
                            let ns = block_at(nx, y, nz);
                            if FluidKind::from_state_id(ns) == Some(kind) {
                                let nl =
                                    FluidKind::level_from_state_id(ns).expect("fluid just matched");
                                highest = highest.max(FluidKind::amount_of(nl));
                            }
                        }
                        let k = highest - dropoff;
                        if k <= 0 {
                            apply(FluidUpdate {
                                x,
                                y,
                                z,
                                state_id: 0, // 消散（vanilla：置空气）
                            });
                            continue; // 消散后不再扩散
                        }
                        FluidKind::level_of(k)
                    };
                    if new_level != current {
                        current = new_level;
                        current_amount = FluidKind::amount_of(current);
                        apply(FluidUpdate {
                            x,
                            y,
                            z,
                            state_id: kind.state_id(current),
                        });
                    }
                }

                // 2. 扩散（vanilla `FlowingFluid.spread`）：下方可替换 → 下落为
                //    falling（LEVEL 8）；下方为实体（非流体非空气）→ 水平扩散
                //    （amount - dropoff，falling 以 amount 7 铺开）；下方为同流体
                //    → 不扩散（vanilla `isWaterHole` 命中）。
                let below = block_at(x, y - 1, z);
                let below_kind = FluidKind::from_state_id(below);
                let below_spreadable =
                    below == 0 || (below_kind.is_some() && below_kind != Some(kind));
                if below_spreadable {
                    apply(FluidUpdate {
                        x,
                        y: y - 1,
                        z,
                        state_id: kind.state_id(8), // falling
                    });
                    // 源块 + 至少 3 个源邻居：下落的同时也向侧边扩散
                    // （vanilla `spread` 的 sourceNeighborCount >= 3 分支）
                    if is_source
                        && source_neighbor_count(
                            &mut block_at,
                            x,
                            y,
                            z,
                            min_x,
                            min_z,
                            max_x,
                            max_z,
                            kind,
                            directions,
                        ) >= 3
                    {
                        spread_to_sides(
                            &mut block_at,
                            &mut apply,
                            x,
                            y,
                            z,
                            kind,
                            current_amount,
                            dropoff,
                            false,
                            min_x,
                            min_z,
                            max_x,
                            max_z,
                            directions,
                        );
                    }
                } else if below_kind != Some(kind) {
                    // 下方是实体方块：源块/流动态/falling 一律向侧边扩散
                    // （vanilla：solid below → `!isWaterHole` → spreadToSides）
                    spread_to_sides(
                        &mut block_at,
                        &mut apply,
                        x,
                        y,
                        z,
                        kind,
                        current_amount,
                        dropoff,
                        FluidKind::is_falling(current),
                        min_x,
                        min_z,
                        max_x,
                        max_z,
                        directions,
                    );
                }
            }
        }
    }
}

/// 水平方向同流体源块（LEVEL 0）数量（vanilla `sourceNeighborCount`）。
#[allow(clippy::too_many_arguments)]
fn source_neighbor_count(
    block_at: &mut impl FnMut(i32, i32, i32) -> i32,
    x: i32,
    y: i32,
    z: i32,
    min_x: i32,
    min_z: i32,
    max_x: i32,
    max_z: i32,
    kind: FluidKind,
    directions: [(i32, i32); 4],
) -> i32 {
    let mut count = 0;
    for (dx, dz) in directions {
        let nx = x + dx;
        let nz = z + dz;
        if nx < min_x || nx > max_x || nz < min_z || nz > max_z {
            continue;
        }
        let ns = block_at(nx, y, nz);
        if FluidKind::from_state_id(ns) == Some(kind)
            && FluidKind::level_from_state_id(ns) == Some(0)
        {
            count += 1;
        }
    }
    count
}

/// 水平扩散（vanilla `spreadToSides`）：扩散强度 = amount - dropoff
/// （falling 以 7 铺开），向四周可替换位置放置流动态。
#[allow(clippy::too_many_arguments)]
fn spread_to_sides(
    block_at: &mut impl FnMut(i32, i32, i32) -> i32,
    apply: &mut impl FnMut(FluidUpdate),
    x: i32,
    y: i32,
    z: i32,
    kind: FluidKind,
    amount: i32,
    dropoff: i32,
    falling: bool,
    min_x: i32,
    min_z: i32,
    max_x: i32,
    max_z: i32,
    directions: [(i32, i32); 4],
) {
    let side_amount = if falling { 7 } else { amount - dropoff };
    if side_amount <= 0 {
        return; // 边缘（amount 1 / 岩浆 amount ≤ 2）不再扩散
    }
    let side_level = FluidKind::level_of(side_amount);
    let side_state = kind.state_id(side_level);
    for (dx, dz) in directions {
        let nx = x + dx;
        let nz = z + dz;
        if nx < min_x || nx > max_x || nz < min_z || nz > max_z {
            continue;
        }
        let ns = block_at(nx, y, nz);
        let neighbor_kind = FluidKind::from_state_id(ns);
        if ns == 0 || (neighbor_kind.is_some() && neighbor_kind != Some(kind)) {
            apply(FluidUpdate {
                x: nx,
                y,
                z: nz,
                state_id: side_state,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FluidKind, FluidUpdate, block_state_ids, tick_fluid};

    fn run_ticks(
        blocks: &mut std::collections::HashMap<(i32, i32, i32), i32>,
        ticks: usize,
        min_x: i32,
        min_z: i32,
        max_x: i32,
        max_z: i32,
    ) {
        for _ in 0..ticks {
            let mut updates: Vec<FluidUpdate> = Vec::new();
            tick_fluid(
                min_x,
                -64,
                min_z,
                max_x,
                319,
                max_z,
                |x, y, z| *blocks.get(&(x, y, z)).unwrap_or(&0),
                |update: FluidUpdate| updates.push(update),
            );
            for update in updates {
                if update.state_id == 0 {
                    blocks.remove(&(update.x, update.y, update.z));
                } else {
                    blocks.insert((update.x, update.y, update.z), update.state_id);
                }
            }
        }
    }

    #[test]
    fn fluid_kind_mapping() {
        assert_eq!(
            FluidKind::from_state_id(block_state_ids::WATER),
            Some(FluidKind::Water)
        );
        assert_eq!(
            FluidKind::from_state_id(block_state_ids::WATER + 7),
            Some(FluidKind::Water)
        );
        assert_eq!(
            FluidKind::from_state_id(block_state_ids::LAVA),
            Some(FluidKind::Lava)
        );
        assert_eq!(
            FluidKind::from_state_id(block_state_ids::LAVA + 15),
            Some(FluidKind::Lava)
        );
        assert_eq!(FluidKind::from_state_id(block_state_ids::STONE), None);
        assert_eq!(
            FluidKind::level_from_state_id(block_state_ids::WATER),
            Some(0)
        );
        assert_eq!(
            FluidKind::level_from_state_id(block_state_ids::LAVA + 5),
            Some(5)
        );
        assert_eq!(FluidKind::Water.state_id(3), block_state_ids::WATER + 3);
        assert_eq!(FluidKind::Lava.state_id(15), block_state_ids::LAVA + 15);
        // LEVEL → amount 语义（vanilla）：源 8、流动 8-L、falling 16-L
        assert_eq!(FluidKind::amount_of(0), 8);
        assert_eq!(FluidKind::amount_of(1), 7);
        assert_eq!(FluidKind::amount_of(7), 1);
        assert_eq!(FluidKind::amount_of(8), 8);
        assert_eq!(FluidKind::amount_of(15), 1);
        assert_eq!(FluidKind::level_of(7), 1);
        assert_eq!(FluidKind::level_of(1), 7);
    }

    #[test]
    fn water_pool_spreads_and_stabilizes() {
        // 15×15 平面：中心 (7,0,7) 水源、全平面 y=-1 基岩地面阻挡下落 →
        // 向四周扩散，水 dropoff 1 → 半径 7 的池子稳定（边缘 amount 1 = LEVEL 7）。
        let mut blocks = std::collections::HashMap::new();
        for x in 0..15 {
            for z in 0..15 {
                blocks.insert((x, -1, z), block_state_ids::BEDROCK);
            }
        }
        blocks.insert((7, 0, 7), block_state_ids::WATER);
        run_ticks(&mut blocks, 40, 0, 0, 14, 14);
        // 半径 7 内（曼哈顿距离 ≤ 7 的菱形）全是水（含源），池外无
        for dx in -7..=7 {
            for dz in -7..=7 {
                let state = *blocks.get(&(7 + dx, 0, 7 + dz)).unwrap_or(&0);
                if dx.abs() + dz.abs() <= 7 {
                    assert!(
                        FluidKind::from_state_id(state) == Some(FluidKind::Water),
                        "({dx},{dz}) should be water, got {state}"
                    );
                } else {
                    assert_eq!(state, 0, "({dx},{dz}) outside pool should be air");
                }
            }
        }
        // 边缘（距源 7 格）amount 1 → LEVEL 7
        let edge = *blocks.get(&(7 + 7, 0, 7)).unwrap();
        assert_eq!(FluidKind::level_from_state_id(edge), Some(7));
        // 池外无水
        assert_eq!(blocks.get(&(7 + 8, 0, 7)), None);
        // 源块仍在
        assert_eq!(blocks.get(&(7, 0, 7)), Some(&block_state_ids::WATER));
    }

    #[test]
    fn water_flows_down_as_falling_column() {
        // 1×1 竖井：顶部 y=2 放水源、y=0 下方有地面 → 下落成 falling 柱
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, 2, 0), block_state_ids::WATER);
        blocks.insert((0, -1, 0), block_state_ids::BEDROCK);
        run_ticks(&mut blocks, 20, 0, 0, 0, 0);
        // 整列 0..2 均为水，下落态为 falling（LEVEL 8）
        for y in 0..=2 {
            let state = *blocks.get(&(0, y, 0)).unwrap_or(&0);
            assert!(
                FluidKind::from_state_id(state) == Some(FluidKind::Water),
                "y={y} should be water"
            );
        }
        assert_eq!(
            FluidKind::level_from_state_id(*blocks.get(&(0, 1, 0)).unwrap()),
            Some(8)
        );
        assert_eq!(
            FluidKind::level_from_state_id(*blocks.get(&(0, 2, 0)).unwrap()),
            Some(0)
        );
    }

    #[test]
    fn water_recedes_without_source() {
        // 孤立流动态（无源）：下一 tick 消散为空气
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, 0, 0), block_state_ids::WATER + 1); // 孤立的 LEVEL 1
        run_ticks(&mut blocks, 3, 0, 0, 0, 0);
        assert_eq!(blocks.get(&(0, 0, 0)), None, "isolated flow must drain");
    }

    #[test]
    fn lava_flows_three_blocks() {
        // 岩浆源 + 全地面：dropoff 2 → 平地铺 3 格（LEVEL 2/4/6），第 4 格无水
        let mut blocks = std::collections::HashMap::new();
        for x in 0..12 {
            blocks.insert((x, -1, 0), block_state_ids::BEDROCK);
        }
        blocks.insert((0, 0, 0), block_state_ids::LAVA);
        run_ticks(&mut blocks, 40, 0, 0, 11, 0);
        assert!(blocks.get(&(1, 0, 0)).is_some(), "lava 1 block");
        assert!(blocks.get(&(2, 0, 0)).is_some(), "lava 2 blocks");
        assert!(blocks.get(&(3, 0, 0)).is_some(), "lava 3 blocks");
        assert_eq!(blocks.get(&(4, 0, 0)), None, "lava stops at 3 blocks");
    }

    #[test]
    fn tick_fluid_at_spreads_from_source() {
        // 单位置 tick（计划 tick 路径）：源块下方为实体（基岩）→ 侧边扩散
        // （vanilla `spread` 的 else 分支；下方可扩散且源邻居 < 3 时只下落，
        // 见 water_flows_down_as_falling_column）
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, -1, 0), block_state_ids::BEDROCK);
        blocks.insert((0, 0, 0), block_state_ids::WATER); // 源块
        let result = super::tick_fluid_at(0, 0, 0, |x, y, z| *blocks.get(&(x, y, z)).unwrap_or(&0));
        // 4 个水平邻居各放流动态（amount 7 → LEVEL 1）
        assert_eq!(result.len, 4, "4 sides");
        assert!(
            !result.reschedule_self,
            "source unchanged, no self-reschedule"
        );
        for update in &result.updates[..result.len] {
            assert_eq!(update.y, 0);
            assert_eq!(FluidKind::level_from_state_id(update.state_id), Some(1));
        }
    }

    #[test]
    fn tick_fluid_at_falls_when_below_air() {
        // 单位置 tick：源块下方为空气 → 下落为 falling（LEVEL 8），源邻居
        // 不足 3 个时不再侧边扩散（vanilla `spread` 的 canSpreadTo(DOWN) 分支）
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, 0, 0), block_state_ids::WATER); // 源块，下方为空气
        let result = super::tick_fluid_at(0, 0, 0, |x, y, z| *blocks.get(&(x, y, z)).unwrap_or(&0));
        assert_eq!(result.len, 1, "down only");
        let down = result.updates[0];
        assert_eq!((down.x, down.y, down.z), (0, -1, 0));
        assert_eq!(FluidKind::level_from_state_id(down.state_id), Some(8));
    }

    #[test]
    fn tick_fluid_at_drains_isolated_flow() {
        // 单位置 tick：孤立流动态（无源、下方实体）→ 消散为空气
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, -1, 0), block_state_ids::BEDROCK);
        blocks.insert((0, 0, 0), block_state_ids::WATER + 1); // LEVEL 1 孤立流
        let result = super::tick_fluid_at(0, 0, 0, |x, y, z| *blocks.get(&(x, y, z)).unwrap_or(&0));
        assert_eq!(result.len, 1);
        assert_eq!(result.updates[0].state_id, 0, "drains to air");
        assert!(!result.reschedule_self);
    }

    #[test]
    fn tick_fluid_at_reschedules_changed_flow() {
        // 单位置 tick：流动态 level 4 → 邻居源驱动后 level 下降 → reschedule_self
        let mut blocks = std::collections::HashMap::new();
        blocks.insert((0, -1, 0), block_state_ids::BEDROCK);
        blocks.insert((0, 0, 0), block_state_ids::WATER + 4); // LEVEL 4
        blocks.insert((1, 0, 0), block_state_ids::WATER); // 邻居源（amount 8）
        let result = super::tick_fluid_at(0, 0, 0, |x, y, z| *blocks.get(&(x, y, z)).unwrap_or(&0));
        // 自身更新为 higher amount（源邻居 8 - dropoff 1 = 7 → LEVEL 1）
        assert!(result.reschedule_self);
        let self_update = result
            .updates
            .iter()
            .take(result.len)
            .find(|u| (u.x, u.y, u.z) == (0, 0, 0))
            .expect("self update");
        assert_eq!(
            FluidKind::level_from_state_id(self_update.state_id),
            Some(1)
        );
    }
}
