//! Play 阶段处理：进服区块加载、玩家包处理、聊天与命令。

use std::sync::Arc;
use std::time::{Duration, Instant};

use icbm_gameplay_icbm::bmtool::{BmToolTpsData, format_bmtool_tps};
use icbm_gameplay_minecraft::commands::{self, Command};
use icbm_mc_767::codec::Reader;
use icbm_mc_767::packets::play::serverbound::{
    SChatCommand, SChatMessage, SConfirmTeleport, SKeepAlive, SMovePlayerPos, SMovePlayerPosRot,
    SMovePlayerRot, SMovePlayerStatusOnly, SPlayerAction, SSwing,
};
use icbm_mc_767::packets::play_ids;
use icbm_region::RegionId;
use icbm_region::protocol::{DIM_OVERWORLD, encode_chunk_request};
use tracing::{info, warn};

use crate::client::{Client, NetError, Position};
use crate::server::Server;
use crate::world::ChunkPending;

/// 进服时加载初始区块（拉起 region → 请求 → 等待转发完成）。
pub fn load_initial_chunks(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    let (chunk_x, chunk_z) = {
        let position = client.position.lock().expect("position");
        (position.chunk_x(), position.chunk_z())
    };
    let region_id = RegionId::from_overworld_chunk(chunk_x, chunk_z);
    server.ensure_region(region_id)?;

    let radius = client
        .view_distance
        .load(std::sync::atomic::Ordering::Relaxed)
        .clamp(2, 32);
    let side = 2 * radius + 1;
    let total = usize::try_from(side).expect("radius").pow(2);

    {
        let mut world = server.world.lock().expect("world lock");
        let _ = world.chunk_pending.insert(ChunkPending {
            player: *client.uuid.lock().expect("uuid"),
            total,
            received: 0,
            in_batch: false,
        });
    }
    server.ipc.send(
        &region_id.service_name(),
        icbm_region::protocol::kind::CHUNK_REQUEST,
        &encode_chunk_request(DIM_OVERWORLD, chunk_x, chunk_z, radius),
    )?;

    // 通知 region 玩家进入
    server.notify_player_enter(client, &region_id)?;

    // 等待区块转发完成
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let done = server
            .world
            .lock()
            .expect("world lock")
            .chunk_pending
            .is_none();
        if done {
            return Ok(());
        }
        if Instant::now() > deadline {
            server.world.lock().expect("world lock").chunk_pending = None;
            return Err(NetError::RegionTimeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Play 阶段服务端包处理。
pub fn handle_play_packet(
    server: &Server,
    client: &Arc<Client>,
    id: i32,
    reader: &mut Reader<'_>,
) -> Result<(), NetError> {
    match id {
        play_ids::SERVERBOUND_CONFIRM_TELEPORT => {
            let _ = SConfirmTeleport::decode(reader)?;
        }
        play_ids::SERVERBOUND_CHAT_COMMAND => {
            let packet = SChatCommand::decode(reader)?;
            handle_player_command(server, client, &packet.command);
        }
        play_ids::SERVERBOUND_CHAT => {
            let packet = SChatMessage::decode(reader)?;
            broadcast_chat(server, client, &packet.message);
        }
        play_ids::SERVERBOUND_CHUNK_BATCH_RECEIVED => {
            let _ = icbm_mc_767::packets::play::serverbound::SChunkBatchReceived::decode(reader)?;
        }
        play_ids::SERVERBOUND_CLIENT_COMMAND => {
            let _ = icbm_mc_767::packets::play::serverbound::SClientCommand::decode(reader)?;
        }
        play_ids::SERVERBOUND_CLIENT_INFORMATION => {
            let info = icbm_mc_767::packets::play::serverbound::SClientInformation::decode(reader)?;
            client.view_distance.store(
                i32::from(info.view_distance),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        play_ids::SERVERBOUND_KEEP_ALIVE => {
            let _ = SKeepAlive::decode(reader)?;
        }
        play_ids::SERVERBOUND_MOVE_POS => {
            let packet = SMovePlayerPos::decode(reader)?;
            update_position(server, client, |pos| {
                pos.x = packet.x;
                pos.y = packet.feet_y;
                pos.z = packet.z;
            })?;
        }
        play_ids::SERVERBOUND_MOVE_POS_ROT => {
            let packet = SMovePlayerPosRot::decode(reader)?;
            update_position(server, client, |pos| {
                pos.x = packet.x;
                pos.y = packet.feet_y;
                pos.z = packet.z;
                pos.yaw = packet.yaw;
                pos.pitch = packet.pitch;
            })?;
        }
        play_ids::SERVERBOUND_MOVE_ROT => {
            let packet = SMovePlayerRot::decode(reader)?;
            update_position(server, client, |pos| {
                pos.yaw = packet.yaw;
                pos.pitch = packet.pitch;
            })?;
        }
        play_ids::SERVERBOUND_MOVE_STATUS_ONLY => {
            let _ = SMovePlayerStatusOnly::decode(reader)?;
        }
        play_ids::SERVERBOUND_PLAYER_ACTION => {
            let _ = SPlayerAction::decode(reader)?;
        }
        play_ids::SERVERBOUND_PONG => {
            let _ = icbm_mc_767::packets::play::serverbound::SPong::decode(reader)?;
        }
        play_ids::SERVERBOUND_SWING => {
            let _ = SSwing::decode(reader)?;
        }
        _ => {
            let name = client.name.lock().expect("name").clone();
            warn!(id, name = %name, "unhandled play packet");
        }
    }
    Ok(())
}

/// 更新玩家位置；跨 region 时通知新旧 region。
fn update_position(
    server: &Server,
    client: &Arc<Client>,
    apply: impl FnOnce(&mut Position),
) -> Result<(), NetError> {
    let old_region = {
        let position = client.position.lock().expect("position");
        RegionId::from_overworld_chunk(position.chunk_x(), position.chunk_z())
    };
    let new_region = {
        let mut position = client.position.lock().expect("position");
        apply(&mut position);
        RegionId::from_overworld_chunk(position.chunk_x(), position.chunk_z())
    };
    if old_region != new_region {
        server.notify_player_leave(client, &old_region)?;
        server.ensure_region(new_region)?;
        server.notify_player_enter(client, &new_region)?;
    }
    Ok(())
}

/// 广播聊天。
fn broadcast_chat(server: &Server, client: &Arc<Client>, message: &str) {
    if message.is_empty() || message.len() > 256 {
        return;
    }
    let name = client.name.lock().expect("name").clone();
    let formatted = format!("<{name}> {message}");
    info!("{formatted}");
    let players: Vec<Arc<Client>> = {
        let world = server.world.lock().expect("world lock");
        world.players.values().cloned().collect()
    };
    for player in players {
        let _ = player.send_system_chat(&formatted, false);
    }
}

/// 玩家输入的命令（聊天框 `/`）。
fn handle_player_command(server: &Server, client: &Arc<Client>, command: &str) {
    let line = format!("/{command}");
    match commands::parse_command(&line) {
        Some(Command::GameMode { mode, target }) => {
            let feedback = crate::console::apply_gamemode(server, target.as_deref(), mode);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::GameRule { rule, value }) => {
            let mut world = server.world.lock().expect("world lock");
            let feedback = commands::apply_gamerule(&mut world.gamerules, &rule, value.as_deref());
            let _ = client.send_system_chat(&feedback.unwrap_or_else(|err| err), false);
        }
        Some(Command::Help) => {
            let _ = client.send_system_chat("可用命令: /gamemode, /gamerule, /bmtool tps", false);
        }
        None => {}
    }
    if icbm_gameplay_icbm::bmtool::parse_bmtool(&line)
        == Some(icbm_gameplay_icbm::bmtool::BmToolCommand::Tps)
    {
        let position = client.position.lock().expect("position");
        let data = bmtool_tps_data(server, Some((position.chunk_x(), position.chunk_z())));
        let _ = client.send_system_chat(&format_bmtool_tps(&data), false);
    }
}

/// 聚合 `/bmtool tps` 数据（`current_region` 为玩家所在区块，无玩家时 `None`）。
pub fn bmtool_tps_data(server: &Server, current_region: Option<(i32, i32)>) -> BmToolTpsData {
    let (active, sleeping, avg, current) = {
        let world = server.world.lock().expect("world lock");
        let (active, sleeping) = world.regions.counts();
        let avg = world.regions.avg_tps();
        let current = current_region
            .map(|(cx, cz)| RegionId::from_overworld_chunk(cx, cz))
            .and_then(|id| world.regions.get(&id))
            .map_or(avg, |entry| entry.tps);
        (active, sleeping, avg, current)
    };
    BmToolTpsData {
        cpu_load: read_cpu_load(),
        cpu_cores: std::thread::available_parallelism().map_or(1, usize::from),
        active_regions: active,
        sleeping_regions: sleeping,
        current_region_tps: current,
        avg_region_tps: avg,
    }
}

/// 读取 `/proc/loadavg` 最近 1 分钟负载。
fn read_cpu_load() -> f32 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.0)
}
