//! 控制台：stdin 线程 + 命令分发。
//!
//! 命令来源：minecraft 命名空间（`/gamemode`、`/gamerule`、`/help`）、
//! icbm 命名空间（`/bmtool tps`）、core 命令（`stop`、`list`、`regions`、
//! `say`）。输出经 `tracing` 打印。

use std::sync::atomic::Ordering;

use icbm_gameplay_icbm::bmtool::{BmToolCommand, format_bmtool_tps};
use icbm_gameplay_minecraft::commands::{self, Command, GameMode};
use icbm_mc_767::packets::play::clientbound::{
    CGameEvent, CPlayerAbilities, CPlayerInfoUpdate, PlayerInfoEntry,
};
use tracing::info;

use crate::server::Server;

/// 启动控制台线程（stdin 逐行）。
pub fn spawn_console(server: &Server) {
    let server = server.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if !server.running.load(Ordering::Relaxed) {
                break;
            }
            server.handle_command(&line);
        }
    });
}

impl Server {
    /// 处理一行控制台命令。
    pub fn handle_command(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(command) = commands::parse_command(line) {
            match command {
                Command::GameMode { mode, target } => {
                    let feedback = apply_gamemode(self, target.as_deref(), mode);
                    info!("{feedback}");
                }
                Command::GameRule { rule, value } => {
                    let mut world = self.world.lock().expect("world lock");
                    let feedback =
                        commands::apply_gamerule(&mut world.gamerules, &rule, value.as_deref());
                    info!("{}", feedback.unwrap_or_else(|err| err));
                }
                Command::Help => print_help(),
            }
            return;
        }
        if icbm_gameplay_icbm::bmtool::parse_bmtool(line) == Some(BmToolCommand::Tps) {
            let data = crate::net::play::bmtool_tps_data(self, None);
            info!("{}", format_bmtool_tps(&data));
            return;
        }
        match line {
            "stop" => {
                info!("服务器停止中...");
                self.stop();
            }
            "list" => {
                let world = self.world.lock().expect("world lock");
                if world.players.is_empty() {
                    info!("当前没有在线玩家");
                } else {
                    let names: Vec<String> = world
                        .players
                        .values()
                        .map(|c| c.name.lock().expect("name").clone())
                        .collect();
                    info!("在线玩家 ({}): {}", names.len(), names.join(", "));
                }
            }
            "regions" => {
                let world = self.world.lock().expect("world lock");
                let (active, sleeping) = world.regions.counts();
                info!(
                    "regions: 活动 {active} / 沉睡 {sleeping} / 总数 {}",
                    world.regions.len()
                );
            }
            "help" => print_help(),
            other => {
                if let Some(message) = other.strip_prefix("say ") {
                    broadcast_say(self, message);
                } else {
                    info!("未知命令: {other} (输入 help 查看帮助)");
                }
            }
        }
    }
}

/// 打印帮助。
fn print_help() {
    info!("可用命令:");
    info!("  gamemode <survival|creative|adventure|spectator> [玩家]");
    info!("  gamerule <规则> [值]");
    info!("  bmtool tps");
    info!("  list | regions | say <消息> | stop | help");
}

/// 设置游戏模式（目标缺省为第一个在线玩家）。
#[must_use]
pub fn apply_gamemode(server: &Server, target: Option<&str>, mode: GameMode) -> String {
    let client = {
        let world = server.world.lock().expect("world lock");
        target.map_or_else(
            || world.players.values().next().cloned(),
            |name| {
                world
                    .players
                    .values()
                    .find(|c| c.name.lock().expect("name").as_str() == name)
                    .cloned()
            },
        )
    };
    let Some(client) = client else {
        return target.map_or_else(
            || "没有在线玩家".to_owned(),
            |name| format!("找不到玩家 {name}"),
        );
    };

    *client.gamemode.lock().expect("gamemode") = mode;
    let flags = match mode {
        GameMode::Creative => CPlayerAbilities::FLAG_ALLOW_FLYING | CPlayerAbilities::FLAG_CREATIVE,
        GameMode::Spectator => {
            CPlayerAbilities::FLAG_INVULNERABLE
                | CPlayerAbilities::FLAG_FLYING
                | CPlayerAbilities::FLAG_ALLOW_FLYING
                | CPlayerAbilities::FLAG_CREATIVE
        }
        _ => 0,
    };
    let _ = client.send_encoded(CPlayerAbilities::packet_id(), |w| {
        CPlayerAbilities {
            flags,
            flying_speed: 0.05,
            fov_modifier: 0.1,
        }
        .encode(w);
    });
    let latency = client.latency.load(Ordering::Relaxed);
    let _ = client.send_encoded(CPlayerInfoUpdate::packet_id(), |w| {
        CPlayerInfoUpdate {
            players: vec![PlayerInfoEntry {
                uuid: *client.uuid.lock().expect("uuid"),
                name: client.name.lock().expect("name").clone(),
                game_mode: i32::from(mode.id()),
                listed: true,
                latency,
            }],
        }
        .encode(w);
    });
    let _ = client.send_encoded(CGameEvent::packet_id(), |w| {
        CGameEvent {
            event: 3,
            value: f32::from(mode.id()),
        }
        .encode(w);
    });
    let name = client.name.lock().expect("name").clone();
    format!("已将 {name} 的游戏模式设为 {}", mode.name())
}

/// 广播系统消息（`say`）。
fn broadcast_say(server: &Server, message: &str) {
    let formatted = format!("[服务器] {message}");
    info!("{formatted}");
    let players: Vec<std::sync::Arc<crate::client::Client>> = {
        let world = server.world.lock().expect("world lock");
        world.players.values().cloned().collect()
    };
    for player in players {
        let _ = player.send_system_chat(&formatted, false);
    }
}
