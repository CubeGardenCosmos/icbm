//! 玩家物品栏（`PlayerInventory`）与容器点击处理（vanilla `InventoryMenu`/
//! `AbstractContainerMenu.clicked` 简化）。
//!
//! 槽号（`InventoryMenu`，对照 `PlayerScreenHandler`）：热栏 36..44、主栏
//! 9..35、装备 5..8、合成 1..4、合成输出 0、副手 45；`-999` = 窗口外。
//!
//! 本模块只做**纯计算**（输入槽状态 + 点击事件 → 输出新槽状态与需同步的
//! 槽列表），发包/持久化由 core 编排。

use icbm_mc_767::packets::play::clientbound::CItemStack;

/// 玩家物品栏槽数（热栏 9 + 主栏 27 + 装备 4 + 副手 1 = 41 可存 + 合成区 5）。
pub const INVENTORY_SLOTS: usize = 46;

/// 热栏槽范围（`InventoryMenu`）。
pub const HOTBAR_START: usize = 36;
pub const HOTBAR_END: usize = 45; // 不含（44 是最后热栏槽）
/// 主栏槽范围。
pub const MAIN_START: usize = 9;
pub const MAIN_END: usize = 36;
/// 副手槽。
pub const OFFHAND_SLOT: usize = 45;
/// 窗口外槽位。
pub const OUTSIDE_SLOT: i16 = -999;

/// 槽内容（`CItemStack` 已含 item/count，空栈 = 空气）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot(pub CItemStack);

impl Default for Slot {
    fn default() -> Self {
        Self(CItemStack::empty())
    }
}

/// 玩家物品栏：46 槽 + 光标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInventory {
    /// 槽（`InventoryMenu` 槽号 0..45）。
    pub slots: Vec<Slot>,
    /// 光标物品。
    pub carried: Slot,
}

impl Default for PlayerInventory {
    fn default() -> Self {
        Self {
            slots: (0..INVENTORY_SLOTS)
                .map(|_| Slot(CItemStack::empty()))
                .collect(),
            carried: Slot(CItemStack::empty()),
        }
    }
}

impl PlayerInventory {
    /// 新建空物品栏。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 槽位是否有效（0..46）。
    #[must_use]
    pub const fn is_valid_slot(slot: i16) -> bool {
        slot >= 0 && (slot as i32) < INVENTORY_SLOTS as i32
    }

    /// 槽是否在热栏。
    #[must_use]
    pub const fn is_hotbar(slot: usize) -> bool {
        slot >= HOTBAR_START && slot < HOTBAR_END
    }

    /// 槽是否在主栏。
    #[must_use]
    pub const fn is_main(slot: usize) -> bool {
        slot >= MAIN_START && slot < MAIN_END
    }

    /// 槽内容（调用方保证 `slot` 有效）。
    #[must_use]
    pub fn slot(&self, slot: usize) -> &Slot {
        &self.slots[slot]
    }

    /// 槽内容（可变；调用方保证 `slot` 有效）。
    pub fn slot_mut(&mut self, slot: usize) -> &mut Slot {
        &mut self.slots[slot]
    }

    /// 当前选中热栏槽。
    #[must_use]
    pub fn hotbar_slot(&self, index: usize) -> &Slot {
        self.slot(HOTBAR_START + index.min(8))
    }
}

/// 点击类型（vanilla `ClickType` 序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickType {
    /// 0：鼠标点击（左/右，按钮 0/1）。
    Pickup,
    /// 1：shift 快速移动。
    QuickMove,
    /// 2：数字键/F 交换（按钮 0..8 热栏、40 副手）。
    Swap,
    /// 3：创造中键复制。
    Clone,
    /// 4：丢出（按钮 0 丢 1 个、1 丢整组；Q 或窗口外）。
    Throw,
    /// 5：拖拽（按钮 0/4/8 开始、1/5/9 添加、2/6/10 结束）。
    QuickCraft,
    /// 6：双击拾取全部。
    PickupAll,
}

impl ClickType {
    /// 从协议 `VarInt` 解析。
    #[must_use]
    pub const fn from_id(id: i8) -> Option<Self> {
        match id {
            0 => Some(Self::Pickup),
            1 => Some(Self::QuickMove),
            2 => Some(Self::Swap),
            3 => Some(Self::Clone),
            4 => Some(Self::Throw),
            5 => Some(Self::QuickCraft),
            6 => Some(Self::PickupAll),
            _ => None,
        }
    }
}

/// 一次点击事件（由 `SContainerClick` 解码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClickEvent {
    /// 槽位（-999 = 窗口外）。
    pub slot: i16,
    /// 按钮。
    pub button: i8,
    /// 点击类型。
    pub click_type: ClickType,
}

/// 点击处理结果：变更后的物品栏 + 需同步回客户端的槽（`CSetSlot`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickResult {
    /// 变更后的物品栏。
    pub inventory: PlayerInventory,
    /// 需同步的槽（槽号列表；core 据此逐个发 `CSetSlot`）。
    pub changed_slots: Vec<i32>,
    /// 光标也变化了（需同步 `CSetSlot` 槽 -1 或全量）。
    pub carried_changed: bool,
}

/// 处理一次容器点击（vanilla `AbstractContainerMenu.clicked` 简化）。
///
/// 支持：`PICKUP`（左/右交换、同物品合并）、`QUICK_MOVE`（shift 热栏↔主栏）、
/// `SWAP`（数字键热栏/F 副手）、`THROW`（丢出）。`QUICK_CRAFT`/`CLONE`/
/// `PICKUP_ALL` 暂不实现（记录差异进 parity matrix）。
#[must_use]
pub fn handle_click(inventory: &PlayerInventory, event: ClickEvent) -> ClickResult {
    let mut inv = inventory.clone();
    let mut changed: Vec<i32> = Vec::new();
    let mut carried_changed = false;

    let push = |slot: i32, changed: &mut Vec<i32>| {
        if !changed.contains(&slot) {
            changed.push(slot);
        }
    };

    match event.click_type {
        ClickType::Pickup => {
            // 按钮 0 = 左（整组/半组）、1 = 右（1 个/半组）
            if event.slot == OUTSIDE_SLOT {
                // 窗口外点击：丢出光标
                if !inv.carried.0.is_empty() {
                    inv.carried = Slot(CItemStack::empty());
                    carried_changed = true;
                }
            } else if PlayerInventory::is_valid_slot(event.slot) {
                let slot = usize::try_from(event.slot).unwrap_or(0);
                apply_pickup(&mut inv, slot, event.button, &mut changed);
                carried_changed = true;
            }
        }
        ClickType::QuickMove => {
            if PlayerInventory::is_valid_slot(event.slot) {
                let slot = usize::try_from(event.slot).unwrap_or(0);
                apply_quick_move(&mut inv, slot, &mut changed);
            }
        }
        ClickType::Swap => {
            if PlayerInventory::is_valid_slot(event.slot) {
                let slot = usize::try_from(event.slot).unwrap_or(0);
                // 按钮 0..8 = 数字键（与热栏交换）；40 = F（与副手交换）
                let swap_slot = if (0..=8).contains(&event.button) {
                    HOTBAR_START + usize::try_from(event.button).unwrap_or(0)
                } else if event.button == 40 {
                    OFFHAND_SLOT
                } else {
                    return ClickResult {
                        inventory: inv,
                        changed_slots: changed,
                        carried_changed,
                    };
                };
                inv.slots.swap(slot, swap_slot);
                push(i32::try_from(slot).unwrap_or(0), &mut changed);
                push(i32::try_from(swap_slot).unwrap_or(0), &mut changed);
            }
        }
        ClickType::Throw => {
            // 按钮 0 = 丢 1 个、1 = 丢整组；窗口外 = 丢光标
            if event.slot == OUTSIDE_SLOT {
                if !inv.carried.0.is_empty() {
                    if event.button == 1 {
                        inv.carried = Slot(CItemStack::empty());
                    } else {
                        let mut stack = inv.carried.0.clone();
                        stack.count = stack.count.saturating_sub(1).max(0);
                        inv.carried = Slot(stack);
                    }
                    carried_changed = true;
                }
            } else if PlayerInventory::is_valid_slot(event.slot) {
                let slot = usize::try_from(event.slot).unwrap_or(0);
                let stack = inv.slot_mut(slot);
                if event.button == 1 {
                    *stack = Slot(CItemStack::empty());
                } else {
                    stack.0.count = stack.0.count.saturating_sub(1).max(0);
                    if stack.0.count == 0 {
                        *stack = Slot(CItemStack::empty());
                    }
                }
                push(i32::from(event.slot), &mut changed);
            }
        }
        // 暂不支持的类型：原样返回（客户端预期无变化；parity matrix 记录）
        ClickType::Clone | ClickType::QuickCraft | ClickType::PickupAll => {
            return ClickResult {
                inventory: inv,
                changed_slots: changed,
                carried_changed,
            };
        }
    }

    ClickResult {
        inventory: inv,
        changed_slots: changed,
        carried_changed,
    }
}

/// PICKUP：交换槽与光标（vanilla `AbstractContainerMenu.clicked` 的 PICKUP 分支）。
/// 左键 = 整组（同物品合并或交换）；右键 = 1 个（同物品合并）或半组（拿起）。
fn apply_pickup(inv: &mut PlayerInventory, slot: usize, button: i8, changed: &mut Vec<i32>) {
    let stack = inv.slot_mut(slot).0.clone();
    let carried = inv.carried.0.clone();
    if stack.is_empty() {
        // 槽空：把光标整组（左）或 1 个（右）放入
        if !carried.is_empty() {
            let move_all = button == 0;
            if move_all {
                inv.slot_mut(slot).0 = carried;
                inv.carried = Slot(CItemStack::empty());
            } else {
                let mut put = carried;
                put.count = 1;
                let rest_count = put.count.saturating_sub(1);
                inv.slot_mut(slot).0 = put;
                inv.carried = if rest_count <= 0 {
                    Slot(CItemStack::empty())
                } else {
                    let mut rest = inv.slot_mut(slot).0.clone();
                    rest.count = rest_count;
                    Slot(rest)
                };
            }
            changed.push(i32::try_from(slot).unwrap_or(0));
        }
    } else if carried.is_empty() {
        // 槽有物品、光标空：拿起整组（左）或半组（右）
        let count = stack.count;
        if button == 0 {
            inv.carried = Slot(stack);
            inv.slot_mut(slot).0 = CItemStack::empty();
        } else {
            let take = (count + 1) / 2;
            let mut taken = stack;
            taken.count = take;
            let rest_count = count - take;
            inv.carried = Slot(taken);
            inv.slot_mut(slot).0 = if rest_count <= 0 {
                CItemStack::empty()
            } else {
                let mut rest = inv.carried.0.clone();
                rest.count = rest_count;
                rest
            };
        }
        changed.push(i32::try_from(slot).unwrap_or(0));
    } else if stack.item == carried.item {
        // 同物品：合并（左 = 全部可放、右 = 1 个）
        let max = 64;
        let space = max - stack.count;
        if space > 0 {
            if button == 0 {
                let move_count = carried.count.min(space);
                inv.slot_mut(slot).0.count = stack.count + move_count;
                let mut rest = carried.clone();
                rest.count = carried.count - move_count;
                inv.carried = if rest.count <= 0 {
                    Slot(CItemStack::empty())
                } else {
                    Slot(rest)
                };
            } else {
                inv.slot_mut(slot).0.count = stack.count + 1;
                let mut rest = carried.clone();
                rest.count = carried.count - 1;
                inv.carried = if rest.count <= 0 {
                    Slot(CItemStack::empty())
                } else {
                    Slot(rest)
                };
            }
            changed.push(i32::try_from(slot).unwrap_or(0));
        }
    } else if carried.count <= 64 {
        // 异物品：交换槽与光标
        inv.slot_mut(slot).0 = carried;
        inv.carried = Slot(stack);
        changed.push(i32::try_from(slot).unwrap_or(0));
    }
}

/// `QUICK_MOVE`：shift 快速移动（热栏↔主栏，vanilla `quickMoveStack` 简化）。
fn apply_quick_move(inv: &mut PlayerInventory, slot: usize, changed: &mut Vec<i32>) {
    let stack = inv.slot_mut(slot).0.clone();
    if stack.is_empty() {
        return;
    }
    // 从热栏移到主栏空槽，或从主栏移到热栏空槽
    let target_range = if PlayerInventory::is_hotbar(slot) {
        MAIN_START..MAIN_END
    } else if PlayerInventory::is_main(slot) {
        HOTBAR_START..HOTBAR_END
    } else {
        return;
    };
    let mut moved = 0;
    for target in target_range {
        if inv.slot(target).0.is_empty() {
            inv.slot_mut(target).0 = stack.clone();
            moved = stack.count;
            inv.slot_mut(slot).0 = CItemStack::empty();
            changed.push(i32::try_from(target).unwrap_or(0));
            break;
        }
    }
    if moved > 0 {
        changed.push(i32::try_from(slot).unwrap_or(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: i32) -> CItemStack {
        CItemStack::of(item, count)
    }

    #[test]
    fn pickup_moves_entire_stack_left_click() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(36).0 = stack("obsidian", 5);
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 36,
                button: 0,
                click_type: ClickType::Pickup,
            },
        );
        assert!(result.inventory.carried.0.item.as_deref() == Some("obsidian"));
        assert_eq!(result.inventory.carried.0.count, 5);
        assert!(result.inventory.slot(36).0.is_empty());
        assert!(result.changed_slots.contains(&36));
        assert!(result.carried_changed);
    }

    #[test]
    fn pickup_right_click_takes_half() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(36).0 = stack("dirt", 5);
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 36,
                button: 1,
                click_type: ClickType::Pickup,
            },
        );
        assert_eq!(result.inventory.carried.0.count, 3); // (5+1)/2
        assert_eq!(result.inventory.slot(36).0.count, 2);
    }

    #[test]
    fn pickup_merges_same_item() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(36).0 = stack("stone", 30);
        inv.carried = Slot(stack("stone", 50));
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 36,
                button: 0,
                click_type: ClickType::Pickup,
            },
        );
        assert_eq!(result.inventory.slot(36).0.count, 64); // 30 + 34 (cap 64)
        assert_eq!(result.inventory.carried.0.count, 16); // 50 - 34
    }

    #[test]
    fn pickup_swaps_different_items() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(36).0 = stack("stone", 1);
        inv.carried = Slot(stack("dirt", 1));
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 36,
                button: 0,
                click_type: ClickType::Pickup,
            },
        );
        assert_eq!(result.inventory.slot(36).0.item.as_deref(), Some("dirt"));
        assert_eq!(result.inventory.carried.0.item.as_deref(), Some("stone"));
    }

    #[test]
    fn quick_move_hotbar_to_main() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(36).0 = stack("stone", 3);
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 36,
                button: 0,
                click_type: ClickType::QuickMove,
            },
        );
        assert!(result.inventory.slot(36).0.is_empty());
        assert_eq!(result.inventory.slot(9).0.item.as_deref(), Some("stone"));
        assert!(result.changed_slots.contains(&36));
        assert!(result.changed_slots.contains(&9));
    }

    #[test]
    fn swap_with_hotbar_key() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(9).0 = stack("stone", 1);
        inv.slot_mut(40).0 = stack("dirt", 1);
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 9,
                button: 4, // 数字键 5 → 热栏槽 40
                click_type: ClickType::Swap,
            },
        );
        assert_eq!(result.inventory.slot(9).0.item.as_deref(), Some("dirt"));
        assert_eq!(result.inventory.slot(40).0.item.as_deref(), Some("stone"));
    }

    #[test]
    fn throw_drops_one() {
        let mut inv = PlayerInventory::new();
        inv.slot_mut(36).0 = stack("stone", 3);
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: 36,
                button: 0,
                click_type: ClickType::Throw,
            },
        );
        assert_eq!(result.inventory.slot(36).0.count, 2);
    }

    #[test]
    fn outside_click_drops_carried() {
        let mut inv = PlayerInventory::new();
        inv.carried = Slot(stack("stone", 1));
        let result = handle_click(
            &inv,
            ClickEvent {
                slot: OUTSIDE_SLOT,
                button: 0,
                click_type: ClickType::Pickup,
            },
        );
        assert!(result.inventory.carried.0.is_empty());
        assert!(result.carried_changed);
    }
}
