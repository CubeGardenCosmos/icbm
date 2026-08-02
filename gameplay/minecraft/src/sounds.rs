//! 声音事件注册表（Phase 1 最小集）。
//!
//! 声音事件在 1.21.1 通过 `minecraft:sound_event` 注册表（客户端内置，
//! 无需下发）。此处提供事件名常量与查询。

/// 声音事件名（`minecraft:` 前缀省略由 [`event`] 补齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundEvent {
    StoneBreak,
    StonePlace,
    GrassBreak,
    GrassPlace,
    DirtBreak,
    DirtPlace,
    WaterAmbient,
    LavaAmbient,
    PortalTravel,
    ExperienceOrbPickup,
    NoteBlock,
    EntityPlayerLevelup,
    BlockNoteBlock,
    UiButtonClick,
}

impl SoundEvent {
    /// 完整事件 ID。
    #[must_use]
    pub const fn event(self) -> &'static str {
        match self {
            Self::StoneBreak => "minecraft:block.stone.break",
            Self::StonePlace => "minecraft:block.stone.place",
            Self::GrassBreak => "minecraft:block.grass.break",
            Self::GrassPlace => "minecraft:block.grass.place",
            Self::DirtBreak => "minecraft:block.gravel.break",
            Self::DirtPlace => "minecraft:block.gravel.place",
            Self::WaterAmbient => "minecraft:ambient.underwater.loop",
            Self::LavaAmbient => "minecraft:block.lava.ambient",
            Self::PortalTravel => "minecraft:block.portal.travel",
            Self::ExperienceOrbPickup => "minecraft:entity.experience_orb.pickup",
            Self::NoteBlock => "minecraft:block.note_block.hat",
            Self::EntityPlayerLevelup => "minecraft:entity.player.levelup",
            Self::BlockNoteBlock => "minecraft:block.note_block.note",
            Self::UiButtonClick => "minecraft:ui.button.click",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SoundEvent;

    #[test]
    fn events_have_minecraft_prefix() {
        for event in [
            SoundEvent::StoneBreak,
            SoundEvent::PortalTravel,
            SoundEvent::UiButtonClick,
        ] {
            assert!(event.event().starts_with("minecraft:"));
        }
    }
}
