//! Play 阶段处理：进服区块加载、玩家包处理、聊天与命令。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use icbm_gameplay_icbm::bmtool::{BmToolTpsData, format_bmtool_tps};
use icbm_gameplay_minecraft::commands::{self, Command};
use icbm_mc_767::codec::Reader;
use icbm_mc_767::packets::play::clientbound::{CCommandSuggestions, CCommands, CommandTreeNode};
use icbm_mc_767::packets::play::serverbound::{
    SChatAck, SChatCommand, SChatCommandSigned, SChatMessage, SCommandSuggestionRequest,
    SConfirmTeleport, SKeepAlive, SMovePlayerPos, SMovePlayerPosRot, SMovePlayerRot,
    SMovePlayerStatusOnly, SPlayerAction, SSwing,
};
use icbm_mc_767::packets::play_ids;
use icbm_region::RegionId;
use icbm_region::protocol::{DIM_OVERWORLD, encode_chunk_request};
use tracing::{debug, info, warn};

use crate::client::{Client, NetError, Position};
use crate::server::Server;
use crate::world::ChunkPending;

/// Y>400 虚拟区：该高度活动不加载/不模拟任何 region（core 接管，对照 README
/// 「Y > 400 活动时不加载任何 region」与 master「Y>400 虚拟服务器」设计）。
pub const VIRTUAL_ZONE_MIN_Y: f64 = 400.0;

/// 服务端视角的区块半径（客户端可视距离只影响其渲染，不扩大服务端发送半径；
/// 对照 vanilla `PlayerManager.viewDistance` 与 `ServerChunkLoadingManager`）。
///
/// 此前进服请求用客户端上报的可视距离（最大 32），65×65=4225 区块按 region
/// 每 tick 8 块约需 26 秒，是进服“长时间坠入虚空”的直接原因之一。
#[must_use]
pub fn effective_view_distance(server: &Server) -> i32 {
    server.config.view_distance.clamp(2, 32)
}

/// 进服时加载初始区块（拉起 region → 请求 → 等待转发完成）。
///
/// 玩家位于 Y>400 虚拟区时 core 接管：不拉起 region、不请求区块、不通知进入；
/// 回落由 [`update_position`] 负责。
pub fn load_initial_chunks(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    let (chunk_x, chunk_z, above) = {
        let position = client.position.lock().expect("position");
        (
            position.chunk_x(),
            position.chunk_z(),
            position.y > VIRTUAL_ZONE_MIN_Y,
        )
    };
    if above {
        debug!(name = %client.name.lock().expect("name"), "join in virtual zone above 400, core takes over");
        return Ok(());
    }
    let region_id = RegionId::from_overworld_chunk(chunk_x, chunk_z);
    server.ensure_region(region_id)?;

    let radius = effective_view_distance(server);
    let side = 2 * radius + 1;
    let total = usize::try_from(side).expect("radius").pow(2);

    {
        let mut world = server.world.lock().expect("world lock");
        let _ = world.chunk_pending.insert(ChunkPending {
            player: *client.uuid.lock().expect("uuid"),
            total,
            received: 0,
            in_batch: false,
            min_x: chunk_x - radius,
            max_x: chunk_x + radius,
            min_z: chunk_z - radius,
            max_z: chunk_z + radius,
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
            break;
        }
        if Instant::now() > deadline {
            server.world.lock().expect("world lock").chunk_pending = None;
            return Err(NetError::RegionTimeout);
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // 以玩家所在 region 为中心预拉相邻 3×3 子 region（启动子进程并生成地形）
    preload_neighbor_regions(server, client);
    Ok(())
}

/// 请求玩家当前位置周边的区块并转发给客户端（移动触发）。
///
/// 对照 vanilla `PlayerChunkWatchingManager`：客户端可视中心移出已发中心
/// `view/2` 时重新请求整个视野，并更新 `SetChunkCacheCenter`。
pub fn request_chunks_around(
    server: &Server,
    client: &Arc<Client>,
    chunk_x: i32,
    chunk_z: i32,
) -> Result<(), NetError> {
    // Y>400 虚拟区不加载 region（core 接管）；调用方已保证，此处防御性跳过
    if client.position.lock().expect("position").y > VIRTUAL_ZONE_MIN_Y {
        return Ok(());
    }
    let region_id = RegionId::from_overworld_chunk(chunk_x, chunk_z);
    server.ensure_region(region_id)?;

    let radius = effective_view_distance(server);
    let side = 2 * radius + 1;
    let total = usize::try_from(side).expect("radius").pow(2);
    {
        let mut world = server.world.lock().expect("world lock");
        let _ = world.chunk_pending.insert(ChunkPending {
            player: *client.uuid.lock().expect("uuid"),
            total,
            received: 0,
            in_batch: false,
            min_x: chunk_x - radius,
            max_x: chunk_x + radius,
            min_z: chunk_z - radius,
            max_z: chunk_z + radius,
        });
    }
    server.ipc.send(
        &region_id.service_name(),
        icbm_region::protocol::kind::CHUNK_REQUEST,
        &encode_chunk_request(DIM_OVERWORLD, chunk_x, chunk_z, radius),
    )?;
    *client.chunk_center.lock().expect("chunk center") = (chunk_x, chunk_z);
    client.send_encoded(
        icbm_mc_767::packets::play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
        |w| {
            icbm_mc_767::packets::play::clientbound::CSetChunkCacheCenter { chunk_x, chunk_z }
                .encode(w);
        },
    )?;
    Ok(())
}

/// 预拉相邻 region：拉起周边 3×3 子 region 并请求少量区块（半径 0）让地形
/// 提前生成，避免玩家跨区时无地可站直接落入虚空。
///
/// 说明：README 的 9×9 预生成超出 datarouter 端口上限（`DATAROUTER_MAX_PORTS`=64，
/// 9×9=81 > 64），Phase 1 改为相邻 3×3（9 个 region，含自身）；更远处由
/// 移动触发的 `request_chunks_around` 按需拉起。预生成区块不设 `ChunkPending`，
/// core 侧按请求区域过滤后丢弃；数据仅为让子进程生成并落盘（region 的
/// `network_chunk_payload` 内存缓存 + 卸载时 `flush` 写回 .mca）。
pub fn preload_neighbor_regions(server: &Server, client: &Arc<Client>) {
    let position = client.position.lock().expect("position");
    let center = RegionId::from_overworld_chunk(position.chunk_x(), position.chunk_z());
    drop(position);
    let world_dir = {
        let world = server.world.lock().expect("world lock");
        world.world_dir.clone()
    };
    for drx in -1..=1 {
        for drz in -1..=1 {
            let id = RegionId {
                rx: center.rx + drx,
                rz: center.rz + drz,
            };
            let known = server
                .world
                .lock()
                .expect("world lock")
                .regions
                .contains(&id);
            if known {
                continue;
            }
            let spawn_result = {
                let mut world = server.world.lock().expect("world lock");
                world.regions.spawn(
                    &id,
                    &world_dir,
                    &server.config.shm_name,
                    server.config.log_level,
                )
            };
            if let Err(err) = spawn_result {
                warn!(id = %id.service_name(), %err, "preload spawn failed");
                continue;
            }
            // 半径 0：只生成 region 中心区块作为“地形已生成”信号，不转发给玩家
            let _ = server.ipc.send(
                &id.service_name(),
                icbm_region::protocol::kind::CHUNK_REQUEST,
                &encode_chunk_request(DIM_OVERWORLD, id.rx * 32 + 16, id.rz * 32 + 16, 0),
            );
        }
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
        play_ids::SERVERBOUND_CHAT_COMMAND_SIGNED => {
            // 安全聊天永不支持：解码签名结构后忽略签名，按未签名命令执行。
            let packet = SChatCommandSigned::decode(reader)?;
            handle_player_command(server, client, &packet.command);
        }
        play_ids::SERVERBOUND_CHAT => {
            let packet = SChatMessage::decode(reader)?;
            broadcast_chat(server, client, &packet.message);
        }
        play_ids::SERVERBOUND_CHAT_ACK => {
            let _ = SChatAck::decode(reader)?;
        }
        play_ids::SERVERBOUND_COMMAND_SUGGESTION => {
            let packet = SCommandSuggestionRequest::decode(reader)?;
            handle_command_suggestion(server, client, &packet)?;
        }
        play_ids::SERVERBOUND_CHUNK_BATCH_RECEIVED => {
            let _ = icbm_mc_767::packets::play::serverbound::SChunkBatchReceived::decode(reader)?;
        }
        play_ids::SERVERBOUND_CLIENT_COMMAND => {
            let _ = icbm_mc_767::packets::play::serverbound::SClientCommand::decode(reader)?;
        }
        play_ids::SERVERBOUND_CUSTOM_PAYLOAD => {
            handle_custom_payload(server, client, reader)?;
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

/// 更新玩家位置；处理跨 region 与 Y>400 虚拟区（core 接管）迁移。
///
/// 归属以 `player_regions` 为准：Y≤400 时记录所在 region；Y>400 时不保留条目
/// （虚拟区不加载/不模拟任何 region，对照 README「Y > 400 活动时不加载任何
/// region」）。升入虚拟区时通知原 region 离开（无玩家即沉睡），回落时拉起
/// 精确落点 region 并立即补视野区块，避免落在未加载地形。
fn update_position(
    server: &Server,
    client: &Arc<Client>,
    apply: impl FnOnce(&mut Position),
) -> Result<(), NetError> {
    let uuid = *client.uuid.lock().expect("uuid");
    let old_region = server
        .world
        .lock()
        .expect("world lock")
        .player_regions
        .get(&uuid)
        .copied();
    let mut position = client.position.lock().expect("position");
    apply(&mut position);
    let y = position.y;
    drop(position);
    client.note_move();

    let (chunk_x, chunk_z) = {
        let position = client.position.lock().expect("position");
        (position.chunk_x(), position.chunk_z())
    };
    let new_region =
        (y <= VIRTUAL_ZONE_MIN_Y).then(|| RegionId::from_overworld_chunk(chunk_x, chunk_z));

    match (old_region, new_region) {
        (Some(old), Some(new)) if old != new => {
            // 跨 region（地面带）：先通知旧区离开，拉起/复用新区并通知进入，
            // 再预拉相邻 3×3 并重发视野区块，避免调入未加载地形。
            server.notify_player_leave(client, &old)?;
            server.ensure_region(new)?;
            server.notify_player_enter(client, &new)?;
            {
                let mut world = server.world.lock().expect("world lock");
                let _ = world.player_regions.insert(uuid, new);
            }
            preload_neighbor_regions(server, client);
            request_chunks_around(server, client, chunk_x, chunk_z)?;
        }
        (Some(old), None) => {
            // 升入 Y>400 虚拟区：core 接管，通知原 region 离开（无玩家即沉睡），
            // 不再加载/请求任何 region 区块。
            server.notify_player_leave(client, &old)?;
            server
                .world
                .lock()
                .expect("world lock")
                .player_regions
                .remove(&uuid);
            debug!(
                name = %client.name.lock().expect("name"),
                y, "player entered virtual zone above 400, core takes over"
            );
        }
        (None, Some(new)) => {
            // 从虚拟区回落：拉起/复用精确落点 region、通知进入并立即补视野
            // 区块，保证玩家落地时有地形可站。
            server.ensure_region(new)?;
            server.notify_player_enter(client, &new)?;
            {
                let mut world = server.world.lock().expect("world lock");
                let _ = world.player_regions.insert(uuid, new);
            }
            preload_neighbor_regions(server, client);
            request_chunks_around(server, client, chunk_x, chunk_z)?;
        }
        (Some(_), Some(_)) | (None, None) => {
            // 同一 region / 均在虚拟区：仅在地面带按区块中心漂移补区块。
            if new_region.is_some() {
                let view = effective_view_distance(server);
                let needs_chunks = {
                    let center = *client.chunk_center.lock().expect("chunk center");
                    (chunk_x - center.0).abs() > view / 2 || (chunk_z - center.1).abs() > view / 2
                };
                if needs_chunks {
                    request_chunks_around(server, client, chunk_x, chunk_z)?;
                }
            }
        }
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
            let (feedback, ok) = {
                let mut world = server.world.lock().expect("world lock");
                let feedback =
                    commands::apply_gamerule(&mut world.gamerules, &rule, value.as_deref());
                let ok = feedback.is_ok();
                (feedback.unwrap_or_else(|err| err), ok)
            };
            let _ = client.send_system_chat(&feedback, false);
            if ok {
                server.persist_gamerules();
            }
        }
        Some(Command::Help) => {
            let _ = client.send_system_chat("可用命令: /about, /gamemode, /gamerule, /bmtool tps", false);
        }
        Some(Command::About) => {
            let _ = client.send_system_chat(&about_message(), false);
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

/// 顶层命令名（minecraft 命名空间 + icbm 命名空间 `bmtool`；升序，与
/// `command_suggestions` 的排序约定一致）。
const TOP_LEVEL_COMMANDS: &[&str] = &["about", "bmtool", "gamemode", "gamerule", "help"];

/// 构建客户端命令树（0x11 `CommandTreeS2CPacket`）。
///
/// 根节点 → gamemode/gamerule/bmtool/help；参数统一用 `brigadier:string`：
/// 该类型无内置建议源，客户端对参数按 Tab 会发 0x0B 请求、由服务器回补全。
#[must_use]
pub fn build_command_tree() -> CCommands {
    use CommandTreeNode as Node;
    CCommands {
        nodes: vec![
            Node::Root {
                children: vec![1, 2, 3, 4, 5],
            },
            Node::Literal {
                name: "about".to_owned(),
                children: vec![],
            },
            Node::Literal {
                name: "gamemode".to_owned(),
                children: vec![6],
            },
            Node::Literal {
                name: "gamerule".to_owned(),
                children: vec![7],
            },
            Node::Literal {
                name: "bmtool".to_owned(),
                children: vec![8],
            },
            Node::Literal {
                name: "help".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "mode".to_owned(),
                children: vec![9],
            },
            Node::Argument {
                name: "rule".to_owned(),
                children: vec![10],
            },
            Node::Literal {
                name: "tps".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "target".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "value".to_owned(),
                children: vec![],
            },
        ],
    }
}

/// 处理客户端命令补全请求（0x0B `RequestCommandCompletionsC2SPacket` → 0x10）。
fn handle_command_suggestion(
    server: &Server,
    client: &Arc<Client>,
    packet: &SCommandSuggestionRequest,
) -> Result<(), NetError> {
    let (start, length, suggestions) = command_suggestions(server, &packet.command);
    client.send_encoded(
        icbm_mc_767::packets::play_ids::CLIENTBOUND_COMMAND_SUGGESTIONS,
        |w| {
            CCommandSuggestions {
                id: packet.id,
                start,
                length,
                suggestions,
            }
            .encode(w);
        },
    )
}

/// 计算命令补全建议与替换区间。
///
/// 客户端请求文本含前导 `/`（对照 master `handle_command_suggestion` 的
/// `get(1..)`）。`start`/`length` 覆盖正在输入的最后一个词；字面量已完整时
/// （如 `/gamemode`）区间为空、补全其第一个参数。
fn command_suggestions(server: &Server, input: &str) -> (i32, i32, Vec<String>) {
    let Some(body) = input.strip_prefix('/') else {
        return (0, 0, Vec::new());
    };
    let slash = input.len() - body.len();
    if body.is_empty() {
        return (i32::try_from(slash).unwrap_or(0), 0, top_level_commands());
    }
    let tokens: Vec<&str> = body.split_whitespace().collect();
    let ends_with_space = body.ends_with(char::is_whitespace);
    // 正在输入的词：尾部空格 → 空词；单词且是完整命令 → 其第一个参数；否则末词。
    let (context, typing, start): (Vec<&str>, &str, usize) =
        if ends_with_space || (tokens.len() == 1 && is_known_command(tokens[0])) {
            (tokens.clone(), "", input.len())
        } else if let Some((last, rest)) = tokens.split_last() {
            let start = input.rfind(' ').map_or(slash, |i| i + 1);
            (rest.to_vec(), last, start)
        } else {
            (Vec::new(), "", slash)
        };
    let mut candidates = match context.as_slice() {
        [] => top_level_commands(),
        ["gamemode"] => vec!["survival", "creative", "adventure", "spectator"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ["gamemode", _] => online_player_names(server),
        ["gamerule"] => icbm_gameplay_minecraft::gamerules::GAME_RULES
            .iter()
            .map(|rule| rule.name.to_owned())
            .collect(),
        ["gamerule", rule] => gamerule_value_suggestions(rule),
        ["bmtool"] => vec!["tps".to_owned()],
        _ => Vec::new(),
    };
    candidates.retain(|candidate| candidate.starts_with(typing));
    candidates.sort();
    candidates.dedup();
    let length = input.len().saturating_sub(start);
    (
        i32::try_from(start).unwrap_or(0),
        i32::try_from(length).unwrap_or(0),
        candidates,
    )
}

/// 顶层命令名（minecraft 命名空间 + icbm 命名空间）。
fn top_level_commands() -> Vec<String> {
    TOP_LEVEL_COMMANDS
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// token 是否为已知顶层命令（字面量已完整）。
fn is_known_command(token: &str) -> bool {
    TOP_LEVEL_COMMANDS.contains(&token)
}

/// 在线玩家名（`/gamemode <mode> <target>` 的第三段）。
fn online_player_names(server: &Server) -> Vec<String> {
    let world = server.world.lock().expect("world lock");
    world
        .players
        .values()
        .map(|player| player.name.lock().expect("name").clone())
        .collect()
}

/// `/gamerule <rule>` 的值建议：布尔规则给 true/false。
fn gamerule_value_suggestions(rule: &str) -> Vec<String> {
    use icbm_gameplay_minecraft::gamerules::RuleType;
    match icbm_gameplay_minecraft::gamerules::find(rule) {
        Some(game_rule) if game_rule.rule_type == RuleType::Bool => {
            vec!["true".to_owned(), "false".to_owned()]
        }
        _ => Vec::new(),
    }
}

/// `/about` 品牌信息（参考 master `core/src/command/commands/about.rs`，品牌为
/// 大写 ICBM；Git 哈希由 `core/build.rs` 注入）。
pub fn about_message() -> String {
    format!(
        "ICBM Block Game Server, fixable client protocol version.\nGit commit: {}\nDeveloped by CubeGarden Studio, Hosted by BlockMember.\nICBM Server is NOT an open source project or freeware. All right reserved except the gameplay assets.",
        env!("GIT_HASH")
    )
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

/// Play 阶段 `minecraft:register` 与 ICBM 通道处理。
///
/// 对照 ICBM Client 协议 v1：`icbm_client:state_subscription`（C2S）为
/// `VarInt` `protocolVersion` + `Boolean` `enabled`；服务器收到有效订阅且客户端
/// 已声明 `state_snapshot` 接收能力后发送快照。
fn handle_custom_payload(
    server: &Server,
    client: &Arc<Client>,
    reader: &mut Reader<'_>,
) -> Result<(), NetError> {
    let channel = reader.read_string()?;
    let data = reader.read_rest().to_vec();
    match channel.as_str() {
        "minecraft:register" => {
            for ch in crate::client::register_channels(&data) {
                if ch == "icbm_client:state_snapshot" {
                    client.icbm_snapshot_ok.store(true, Ordering::Relaxed);
                }
            }
        }
        "icbm_client:state_subscription" => {
            let mut r = Reader::new(&data);
            let Ok(protocol) = r.read_var_int() else {
                return Ok(());
            };
            if protocol != 1 {
                // 协议版本不匹配：忽略
                return Ok(());
            }
            let Ok(enabled) = r.read_bool() else {
                return Ok(());
            };
            client.icbm_subscribed.store(enabled, Ordering::Relaxed);
            debug!(
                name = %client.name.lock().expect("name"),
                enabled, "icbm state subscription"
            );
            if enabled {
                send_icbm_snapshot(server, client);
            }
        }
        _ => {}
    }
    Ok(())
}

/// 发送 ICBM 状态快照（S2C `icbm_client:state_snapshot`）。
///
/// 字段顺序对照 `StateSnapshotPayload.STREAM_CODEC`：`VarInt` 协议版本 →
/// `VarLong` 序号 → `ResourceLocation` 维度 → `Boolean` 裁切 → 可选 Region（4×VarInt）→
/// 可选 regionId → 可选 processId → 可选速度 → 可选 TPS → `VarInt` 刷新间隔。
pub fn send_icbm_snapshot(server: &Server, client: &Arc<Client>) {
    let position = client.position.lock().expect("position");
    let uuid = *client.uuid.lock().expect("uuid");
    let region_id = RegionId::from_overworld_chunk(position.chunk_x(), position.chunk_z());
    let world = server.world.lock().expect("world lock");
    // Y>400 虚拟区（core 接管）不关联任何 region：快照不报告 region/TPS，
    // 客户端据此显示“无区域”状态。
    let in_region = position.y <= VIRTUAL_ZONE_MIN_Y && world.player_regions.contains_key(&uuid);
    let (region, region_name, process_id, tps) = if in_region {
        world
            .regions
            .get(&region_id)
            .map_or((None, None, None, 0.0), |entry| {
                let min_x = i64::from(region_id.rx) * 32 * 16;
                let min_z = i64::from(region_id.rz) * 32 * 16;
                let max_x = min_x + 32 * 16;
                let max_z = min_z + 32 * 16;
                (
                    Some((min_x, min_z, max_x, max_z)),
                    Some(region_id.service_name()),
                    Some(entry.pid.to_string()),
                    entry.tps,
                )
            })
    } else {
        (None, None, None, 0.0)
    };
    let sequence = client.icbm_sequence.fetch_add(1, Ordering::Relaxed) + 1;
    let speed = *client.speed.lock().expect("speed");
    let _ = client.send_encoded(
        icbm_mc_767::packets::play_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        |w| {
            w.write_string("icbm_client:state_snapshot");
            w.write_var_int(1); // protocolVersion
            w.write_var_long(i64::try_from(sequence).unwrap_or(i64::MAX));
            w.write_string("minecraft:overworld");
            w.write_bool(true); // simulationClipped：区域边界真实存在（客户端据此渲染边界）
            match region {
                Some((min_x, min_z, max_x, max_z)) => {
                    w.write_bool(true);
                    w.write_var_int(i32::try_from(min_x).unwrap_or(i32::MAX));
                    w.write_var_int(i32::try_from(min_z).unwrap_or(i32::MAX));
                    w.write_var_int(i32::try_from(max_x).unwrap_or(i32::MAX));
                    w.write_var_int(i32::try_from(max_z).unwrap_or(i32::MAX));
                }
                None => w.write_bool(false),
            }
            match region_name {
                Some(name) => {
                    w.write_bool(true);
                    w.write_string(&name);
                }
                None => w.write_bool(false),
            }
            match process_id {
                Some(pid) => {
                    w.write_bool(true);
                    w.write_string(&pid);
                }
                None => w.write_bool(false),
            }
            // 玩家移动速度（方块/秒，由移动包估算）
            w.write_bool(true);
            w.write_f64_be(speed);
            w.write_bool(true); // 有 TPS
            w.write_f32_be(tps);
            w.write_var_int(20); // refreshIntervalTicks
        },
    );
}
