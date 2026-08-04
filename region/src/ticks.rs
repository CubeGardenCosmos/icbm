//! 计划 tick 引擎（vanilla `LevelTicks` 等价物）。
//!
//! 对照 `net/minecraft/world/ticks/{LevelTicks,LevelChunkTicks,ScheduledTick,
//! TickPriority}.java`：方块/流体各自的计划 tick 队列，按
//! `triggerTick → priority → subTickOrder` 排序（`ScheduledTick.DRAIN_ORDER`），
//! 每 tick 最多执行 `MAX_TICKS_PER_TICK`（vanilla 65536）个到期 tick，
//! 未到期的顺延到下一 tick。
//!
//! 与 vanilla 的差异（有意简化，记录于 parity matrix）：
//! - region 拥有全部区块（无区块级加载/卸载），故用单个全局堆替代 vanilla
//!   的按区块容器（`LevelChunkTicks`）——对同一 region 内调度行为等价；
//! - 同一 (pos, kind) 只保留一个计划 tick（`UNIQUE_TICK_HASH` 语义），重复
//!   schedule 保留最早的 triggerTick；
//! - 不实现 `willTickThisTick`（同 tick 内重调度检测；当前无方块在同一 tick
//!   内连续调度自身）。

use std::collections::{BinaryHeap, HashSet};

/// tick 优先级（vanilla `TickPriority`：值越小越先执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum TickPriority {
    /// -3
    ExtremelyHigh,
    /// -2
    VeryHigh,
    /// -1
    High,
    /// 0（默认）
    #[default]
    Normal,
    /// 1
    Low,
    /// 2
    VeryLow,
    /// 3
    ExtremelyLow,
}

/// 计划 tick（vanilla `ScheduledTick<T>`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScheduledTick<T> {
    /// tick 类型（方块状态 ID / 流体类型；执行时校验当前位置是否仍匹配）。
    pub kind: T,
    /// 位置。
    pub pos: (i32, i32, i32),
    /// 触发 tick（世界时间）。
    pub trigger_tick: u64,
    /// 优先级。
    pub priority: TickPriority,
    /// 同 tick 内调度顺序（FIFO 决胜；vanilla `subTickOrder`）。
    pub sub_tick_order: u64,
}

impl<T: Eq> Ord for ScheduledTick<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.trigger_tick
            .cmp(&other.trigger_tick)
            .then_with(|| self.priority.cmp(&other.priority))
            .then_with(|| self.sub_tick_order.cmp(&other.sub_tick_order))
    }
}

impl<T: Eq> PartialOrd for ScheduledTick<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 每 tick 计划 tick 处理上限（vanilla `LevelTicks.tick` 的 65536）。
pub const MAX_TICKS_PER_TICK: usize = 65536;

/// 计划 tick 队列（vanilla `LevelTicks<T>`；单 region 全量堆 + (pos, kind) 去重）。
#[derive(Debug, Default)]
pub struct LevelTicks<T> {
    /// 按 `ScheduledTick.DRAIN_ORDER` 排序的最小堆。
    heap: BinaryHeap<ReverseTick<T>>,
    /// (pos, kind) 去重集（`UNIQUE_TICK_HASH` 语义）。
    scheduled: HashSet<(i32, i32, i32, T)>,
}

impl<T> LevelTicks<T> {
    /// 空队列。
    #[must_use]
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
            scheduled: HashSet::new(),
        }
    }

    /// 当前排队 tick 数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.scheduled.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scheduled.is_empty()
    }

    /// (pos, kind) 是否已有计划 tick。
    #[must_use]
    pub fn is_scheduled(&self, pos: (i32, i32, i32), kind: T) -> bool
    where
        T: PartialEq + Eq + std::hash::Hash,
    {
        self.scheduled.contains(&(pos.0, pos.1, pos.2, kind))
    }
}

impl<T: PartialEq + Eq + std::hash::Hash + Copy> LevelTicks<T> {
    /// 入队计划 tick（vanilla `LevelTicks.schedule`）：
    /// 同 (pos, kind) 已有计划时忽略本次（保留最早 triggerTick）。
    pub fn schedule(&mut self, tick: ScheduledTick<T>) {
        let key = (tick.pos.0, tick.pos.1, tick.pos.2, tick.kind);
        if self.scheduled.insert(key) {
            self.heap.push(ReverseTick(tick));
        }
    }

    /// 取出到期 tick 列表（vanilla `LevelTicks.tick` 的 collect 阶段）。
    ///
    /// 返回 `triggerTick <= game_time` 的 tick（最多 `max_ticks` 个），未到期
    /// 的留在队列；调用方执行列表内 tick 期间可用 [`schedule`] 重新入队
    /// （重调度的 tick 不会在本 tick 内再次执行，与 vanilla collect→run 一致）。
    #[must_use]
    pub fn drain_due(&mut self, game_time: u64, max_ticks: usize) -> Vec<ScheduledTick<T>> {
        let mut to_run: Vec<ScheduledTick<T>> = Vec::new();
        while to_run.len() < max_ticks {
            let Some(ReverseTick(top)) = self.heap.peek() else {
                break;
            };
            if top.trigger_tick > game_time {
                break;
            }
            let ReverseTick(tick) = self.heap.pop().expect("peeked just now");
            self.scheduled
                .remove(&(tick.pos.0, tick.pos.1, tick.pos.2, tick.kind));
            to_run.push(tick);
        }
        to_run
    }

    /// 执行到期 tick（vanilla `LevelTicks.tick(l, 65536, consumer)` 的便捷封装，
    /// 供不持有外部可变状态的调用方使用）。
    pub fn tick(
        &mut self,
        game_time: u64,
        max_ticks: usize,
        consumer: &mut impl FnMut(T, (i32, i32, i32)),
    ) {
        for tick in self.drain_due(game_time, max_ticks) {
            consumer(tick.kind, tick.pos);
        }
    }
}

/// 最小堆包装（`BinaryHeap` 是最大堆，反转 `ScheduledTick` 的 Ord）。
#[derive(Debug, Clone, Copy)]
struct ReverseTick<T>(ScheduledTick<T>);

impl<T: Eq> PartialEq for ReverseTick<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T: Eq> Eq for ReverseTick<T> {}

impl<T: Eq> PartialOrd for ReverseTick<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Eq> Ord for ReverseTick<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.cmp(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{LevelTicks, ScheduledTick, TickPriority};

    fn tick(
        kind: u8,
        x: i32,
        y: i32,
        z: i32,
        trigger: u64,
        priority: TickPriority,
        order: u64,
    ) -> ScheduledTick<u8> {
        ScheduledTick {
            kind,
            pos: (x, y, z),
            trigger_tick: trigger,
            priority,
            sub_tick_order: order,
        }
    }

    #[test]
    fn drains_due_ticks_in_drain_order() {
        let mut queue = LevelTicks::new();
        // 同 trigger 同 priority：按 sub_tick_order（FIFO）
        queue.schedule(tick(1, 0, 0, 0, 10, TickPriority::Normal, 0));
        queue.schedule(tick(2, 1, 0, 0, 10, TickPriority::Normal, 1));
        // 更早 trigger 的先执行
        queue.schedule(tick(3, 2, 0, 0, 5, TickPriority::Normal, 2));
        // 同 trigger、更高优先级（值更小）先执行
        queue.schedule(tick(4, 3, 0, 0, 10, TickPriority::High, 3));

        let mut ran: Vec<u8> = Vec::new();
        queue.tick(10, super::MAX_TICKS_PER_TICK, &mut |kind, _pos| {
            ran.push(kind)
        });
        assert_eq!(ran, vec![3, 4, 1, 2]);
        assert!(queue.is_empty());
    }

    #[test]
    fn leaves_future_ticks_pending() {
        let mut queue = LevelTicks::new();
        queue.schedule(tick(1, 0, 0, 0, 5, TickPriority::Normal, 0));
        queue.schedule(tick(2, 1, 0, 0, 20, TickPriority::Normal, 1));

        let mut ran: Vec<u8> = Vec::new();
        queue.tick(10, super::MAX_TICKS_PER_TICK, &mut |kind, _pos| {
            ran.push(kind)
        });
        assert_eq!(ran, vec![1]);
        assert_eq!(queue.len(), 1);

        let mut ran2: Vec<u8> = Vec::new();
        queue.tick(20, super::MAX_TICKS_PER_TICK, &mut |kind, _pos| {
            ran2.push(kind)
        });
        assert_eq!(ran2, vec![2]);
        assert!(queue.is_empty());
    }

    #[test]
    fn respects_max_ticks_per_tick() {
        let mut queue = LevelTicks::new();
        for i in 0..5u64 {
            queue.schedule(tick(i as u8, i as i32, 0, 0, 1, TickPriority::Normal, i));
        }
        let mut ran: Vec<u8> = Vec::new();
        queue.tick(1, 2, &mut |kind, _pos| ran.push(kind));
        assert_eq!(ran, vec![0, 1]);
        assert_eq!(queue.len(), 3);
    }

    #[test]
    fn unique_per_position_and_kind() {
        let mut queue = LevelTicks::new();
        // 同 (pos, kind) 重复调度：保留最早 triggerTick（第二次被忽略）
        queue.schedule(tick(1, 0, 0, 0, 5, TickPriority::Normal, 0));
        queue.schedule(tick(1, 0, 0, 0, 100, TickPriority::Normal, 1));
        assert_eq!(queue.len(), 1);
        // 不同 kind 同位置：互不冲突
        queue.schedule(tick(2, 0, 0, 0, 5, TickPriority::Normal, 2));
        assert_eq!(queue.len(), 2);

        let mut ran: Vec<u8> = Vec::new();
        queue.tick(5, super::MAX_TICKS_PER_TICK, &mut |kind, _pos| {
            ran.push(kind)
        });
        assert_eq!(ran, vec![1, 2]);
        assert!(queue.is_empty());
    }

    #[test]
    fn reschedule_after_execution() {
        let mut queue = LevelTicks::new();
        queue.schedule(tick(1, 0, 0, 0, 5, TickPriority::Normal, 0));
        // 执行期间重调度同 (pos, kind)：本 tick 不再执行，下一 tick 生效
        let due = queue.drain_due(5, super::MAX_TICKS_PER_TICK);
        assert_eq!(due.len(), 1);
        for t in &due {
            queue.schedule(tick(
                t.kind,
                t.pos.0,
                t.pos.1,
                t.pos.2,
                10,
                TickPriority::Normal,
                1,
            ));
        }
        assert_eq!(queue.len(), 1);

        let due2 = queue.drain_due(10, super::MAX_TICKS_PER_TICK);
        assert_eq!(due2.iter().map(|t| t.kind).collect::<Vec<_>>(), vec![1]);
        assert!(queue.is_empty());
    }

    #[test]
    fn is_scheduled_reports_pending() {
        let mut queue = LevelTicks::new();
        queue.schedule(tick(1, 0, 0, 0, 5, TickPriority::Normal, 0));
        assert!(queue.is_scheduled((0, 0, 0), 1));
        assert!(!queue.is_scheduled((0, 0, 0), 2));
        assert!(!queue.is_scheduled((1, 0, 0), 1));
    }
}
