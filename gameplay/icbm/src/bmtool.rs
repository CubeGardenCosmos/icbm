//! `/bmtool` 命令。
//!
//! Phase 1 仅实现 `/bmtool tps`，输出：CPU 负载、CPU 核心数量、活跃
//! Region 数量、沉睡 Region 数量、当前 region TPS、平均 region TPS。

/// `/bmtool` 子命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmToolCommand {
    /// `/bmtool tps`。
    Tps,
}

/// 解析 `/bmtool ...` 行；非本命令返回 `None`。
#[must_use]
pub fn parse_bmtool(line: &str) -> Option<BmToolCommand> {
    let line = line.strip_prefix('/').unwrap_or(line).trim();
    let mut parts = line.split_whitespace();
    if parts.next()? != "bmtool" {
        return None;
    }
    match parts.next()? {
        "tps" => Some(BmToolCommand::Tps),
        _ => None,
    }
}

/// `/bmtool tps` 输出所需的数据（由 core 聚合）。
#[derive(Debug, Clone, Copy)]
pub struct BmToolTpsData {
    /// CPU 负载（最近 1 分钟，`/proc/loadavg`）。
    pub cpu_load: f32,
    /// CPU 核心数量。
    pub cpu_cores: usize,
    /// 活跃 Region 数量。
    pub active_regions: usize,
    /// 沉睡 Region 数量。
    pub sleeping_regions: usize,
    /// 当前 region TPS（玩家所在区域；无玩家时取平均值）。
    pub current_region_tps: f32,
    /// 平均 region TPS。
    pub avg_region_tps: f32,
}

/// 格式化 `/bmtool tps` 输出。
#[must_use]
pub fn format_bmtool_tps(data: &BmToolTpsData) -> String {
    format!(
        "CPU负载: {:.2}  CPU核心数量: {}  活跃Region数量: {}  沉睡Region数量: {}  当前region tps: {:.2}  平均region tps: {:.2}",
        data.cpu_load,
        data.cpu_cores,
        data.active_regions,
        data.sleeping_regions,
        data.current_region_tps,
        data.avg_region_tps,
    )
}

#[cfg(test)]
mod tests {
    use super::{BmToolCommand, BmToolTpsData, format_bmtool_tps, parse_bmtool};

    #[test]
    fn parse_tps() {
        assert_eq!(parse_bmtool("/bmtool tps"), Some(BmToolCommand::Tps));
        assert_eq!(parse_bmtool("/bmtool other"), None);
        assert_eq!(parse_bmtool("/bmtool"), None);
        assert_eq!(parse_bmtool("/stop"), None);
    }

    #[test]
    fn format_contains_all_fields() {
        let data = BmToolTpsData {
            cpu_load: 0.42,
            cpu_cores: 8,
            active_regions: 3,
            sleeping_regions: 2,
            current_region_tps: 19.9,
            avg_region_tps: 18.7,
        };
        let output = format_bmtool_tps(&data);
        for field in [
            "CPU负载",
            "CPU核心数量",
            "活跃Region数量",
            "沉睡Region数量",
            "当前region tps",
            "平均region tps",
        ] {
            assert!(output.contains(field), "missing {field}: {output}");
        }
    }
}
