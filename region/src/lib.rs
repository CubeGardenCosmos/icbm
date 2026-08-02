//! 区域模拟服务（`--region` 模式）。
//!
//! 每个 region 进程负责一个主世界 32×32 区块区域（Y<400）与其对应的
//! 地狱 4×4 区块区域，通过 datarouter 与 core 通讯。生命周期状态机：
//! 注册 → 模拟 → 沉睡 → 卸载（见 [`service`]）。

pub mod entities;
pub mod crops;
pub mod fluids;
pub mod mobs;
pub mod portal;
pub mod protocol;
pub mod redstone;
pub mod service;
pub mod store;
pub mod weather;

pub use service::{RegionConfig, RegionError, RegionService, run_region};

/// 维度面（区域进程同时负责主世界面与地狱面）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Face {
    Overworld,
    Nether,
}

/// 主世界 superflat 地表站立 Y（地表顶 -61 + 1；superflat 自洞穴更新后的
/// 最低建筑点 Y=-64 起铺设，见 `icbm_terrain_gen` 的 Overworld flat 配置）。
pub const OVERWORLD_SURFACE_FEET_Y: i32 = -60;

impl Face {
    /// IPC 维度字节。
    #[must_use]
    pub const fn byte(self) -> u8 {
        match self {
            Self::Overworld => protocol::DIM_OVERWORLD,
            Self::Nether => protocol::DIM_NETHER,
        }
    }

    /// 从 IPC 维度字节解析。
    #[must_use]
    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            protocol::DIM_OVERWORLD => Some(Self::Overworld),
            protocol::DIM_NETHER => Some(Self::Nether),
            _ => None,
        }
    }
}

/// 区域 ID（主世界 32×32 区块；地狱面 4×4 按 1:8 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegionId {
    /// 区域 X（主世界区块坐标右移 5 位）。
    pub rx: i32,
    /// 区域 Z。
    pub rz: i32,
}

impl RegionId {
    /// 从主世界区块坐标求区域。
    #[must_use]
    pub const fn from_overworld_chunk(cx: i32, cz: i32) -> Self {
        Self {
            rx: cx >> 5,
            rz: cz >> 5,
        }
    }

    /// 从地狱区块坐标求区域（4×4 → 右移 2 位）。
    #[must_use]
    pub const fn from_nether_chunk(cx: i32, cz: i32) -> Self {
        Self {
            rx: cx >> 2,
            rz: cz >> 2,
        }
    }

    /// 从主世界方块坐标求区域（32 区块 = 512 方块）。
    #[must_use]
    pub const fn from_overworld_block(x: i32, z: i32) -> Self {
        Self::from_overworld_chunk(x.div_euclid(16), z.div_euclid(16))
    }

    /// 从地狱方块坐标求区域（4 区块 = 64 方块）。
    #[must_use]
    pub const fn from_nether_block(x: i32, z: i32) -> Self {
        Self::from_nether_chunk(x.div_euclid(16), z.div_euclid(16))
    }

    /// datarouter 服务名（`icbm.region.o_<rx>_<rz>`）。
    #[must_use]
    pub fn service_name(&self) -> String {
        format!("icbm.region.o_{}_{}", self.rx, self.rz)
    }
}
