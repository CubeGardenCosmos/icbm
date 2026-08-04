//! Play 阶段处理：进服区块加载、玩家包处理、聊天与命令。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use icbm_gameplay_icbm::bmtool::{BmToolTpsData, format_bmtool_tps};
use icbm_gameplay_minecraft::commands::{self, Command};
use icbm_mc_767::codec::Reader;
use icbm_mc_767::packets::play::clientbound::{
    CCommandSuggestions, CCommands, CPlayerPosition, CommandTreeNode,
};
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
use crate::server::forward_chunk_to_pending;
use crate::world::ChunkPending;

/// Y>400 虚拟区：该高度活动不加载/不模拟任何 region（core 接管，对照 README
/// 「Y > 400 活动时不加载任何 region」与 master「Y>400 虚拟服务器」设计）。
pub const VIRTUAL_ZONE_MIN_Y: f64 = 400.0;

/// 主世界最低 Y（`WorldHeight.min_y`，与 `icbm_terrain_gen` 一致）。
/// Y 低于此值视为坠入虚空（无死亡/重生系统，直接拉回出生点）。
pub const OVERWORLD_MIN_Y: f64 = -64.0;

/// 主世界超平坦地表站立 Y（地表顶 -61 上方；superflat 自 Y=-64 铺设）。
pub const OVERWORLD_SURFACE_FEET_Y: f64 = -60.0;

/// 地狱地表站立 Y（flat：bedrock 0、netherrack 1..126、bedrock 127 → 站 128）。
pub const NETHER_SURFACE_FEET_Y: f64 = 128.0;

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
    request_view_chunks(server, client, chunk_x, chunk_z, radius);

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
    // 先更新区块缓存中心并通知客户端，再发起视野请求：core 直读已卸载
    // region .mca 的区块在请求路径内同步转发，若后发 center 会先于
    // center 到达（客户端按 center 消费会吃掉批次开始）。
    *client.chunk_center.lock().expect("chunk center") = (chunk_x, chunk_z);
    client.send_encoded(
        icbm_mc_767::packets::play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
        |w| {
            icbm_mc_767::packets::play::clientbound::CSetChunkCacheCenter { chunk_x, chunk_z }
                .encode(w);
        },
    )?;
    request_view_chunks(server, client, chunk_x, chunk_z, radius);
    Ok(())
}

/// 视野方块 [cx±radius, cz±radius] 覆盖的全部主世界 region（去重）。
///
/// 主世界 region = 32×32 区块；可视半径 8 时最多覆盖 2×2 = 4 个 region。
#[must_use]
fn covering_regions(cx: i32, cz: i32, radius: i32) -> Vec<RegionId> {
    let (min_rx, max_rx) = ((cx - radius) >> 5, (cx + radius) >> 5);
    let (min_rz, max_rz) = ((cz - radius) >> 5, (cz + radius) >> 5);
    let mut regions = Vec::with_capacity(4);
    for rx in min_rx..=max_rx {
        for rz in min_rz..=max_rz {
            regions.push(RegionId { rx, rz });
        }
    }
    regions
}

/// 向视野覆盖的全部 region 请求区块（“区域切换显示”修复核心）。
///
/// - **存活 region（含沉睡）**：core 向其发送 `CHUNK_REQUEST`；region 的
///   `serve_chunks` 只回其拥有的区块（不再把邻区区块以 flat 生成覆盖，避免
///   覆盖客户端已有的真实地形）。沉睡 region 仍在 `tick_sleeping` 中
///   `drain_messages`，可以继续传输地图数据。
/// - **已卸载 region**：core 只读打开其 .mca（卸载 flush 后已落盘）直接转发；
///   区块缺失（从未生成）用 flat 生成兜底（与 region 内存生成一致）。
///
/// 每块区块恰好由一个提供方服务；`ChunkPending` 的总数仍为 `(2r+1)^2`。
/// 各提供方的错误（发送失败/读 .mca 失败）都在内部降级（改读 .mca / flat
/// 兜底），不向上传播——请求永不失败，失败只会导致部分区块晚到。
fn request_view_chunks(
    server: &Server,
    client: &Arc<Client>,
    chunk_x: i32,
    chunk_z: i32,
    radius: i32,
) {
    let radius = radius.clamp(2, 32);
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
    let world_dir = server.world.lock().expect("world lock").world_dir.clone();
    let mut unloaded: Vec<RegionId> = Vec::new();
    for id in covering_regions(chunk_x, chunk_z, radius) {
        let alive = server
            .world
            .lock()
            .expect("world lock")
            .regions
            .get(&id)
            .is_some_and(|entry| entry.alive);
        if alive {
            let send_ok = server
                .ipc
                .send(
                    &id.service_name(),
                    icbm_region::protocol::kind::CHUNK_REQUEST,
                    &icbm_region::protocol::encode_chunk_request(
                        icbm_region::protocol::DIM_OVERWORLD,
                        chunk_x,
                        chunk_z,
                        radius,
                    ),
                )
                .is_ok();
            if send_ok {
                continue;
            }
            warn!(
                id = %id.service_name(),
                "chunk request to live region failed, falling back to .mca read"
            );
        }
        unloaded.push(id);
    }
    // 已卸载 region：core 只读直读 .mca（或 flat 兜底）并转发
    for id in unloaded {
        serve_unloaded_region(server, client, id, chunk_x, chunk_z, radius, &world_dir);
    }
}

/// core 直读已卸载 region 的 .mca：把该 region 与视野方块交集内的每个区块
/// 转发给等待玩家。区块缺失（从未生成）或读取出错时用 flat 生成兜底
/// （与 region 内存生成一致，固定种子可复现）。
fn serve_unloaded_region(
    server: &Server,
    client: &Arc<Client>,
    id: RegionId,
    cx: i32,
    cz: i32,
    radius: i32,
    world_dir: &std::path::Path,
) {
    use icbm_region::protocol::DIM_OVERWORLD;
    use icbm_region::store::{generate_chunk_payload, read_chunk_payload_from_disk};
    let (min_x, max_x) = (
        (id.rx * 32).max(cx - radius),
        (id.rx * 32 + 31).min(cx + radius),
    );
    let (min_z, max_z) = (
        (id.rz * 32).max(cz - radius),
        (id.rz * 32 + 31).min(cz + radius),
    );
    for x in min_x..=max_x {
        for z in min_z..=max_z {
            let payload = match read_chunk_payload_from_disk(world_dir, id.rx, id.rz, x, z) {
                Ok(Some(payload)) => payload,
                Ok(None) | Err(_) => generate_chunk_payload(DIM_OVERWORLD, x, z),
            };
            forward_chunk_to_pending(server, client, x, z, &payload);
        }
    }
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
#[allow(clippy::too_many_lines)] // 包分发 match 逐条对应协议包
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
            client.set_locale(&info.locale);
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
        play_ids::SERVERBOUND_MOVE_VEHICLE => {
            // 乘船/矿车时客户端发送载具移动（0x1E）替代玩家移动包：更新位置
            // 但不做屏障拦截（水面乘船放行，对照需求）。
            let packet = icbm_mc_767::packets::play::serverbound::SMoveVehicle::decode(reader)?;
            update_position(server, client, |pos| {
                pos.x = packet.x;
                pos.y = packet.feet_y;
                pos.z = packet.z;
                pos.yaw = packet.yaw;
                pos.pitch = packet.pitch;
            })?;
            *client
                .last_barrier_check
                .lock()
                .expect("last barrier check") = Instant::now();
        }
        play_ids::SERVERBOUND_PLAYER_ACTION => {
            let packet = SPlayerAction::decode(reader)?;
            handle_player_action(server, client, &packet)?;
        }
        play_ids::SERVERBOUND_PICK_ITEM => {
            // 创造模式中键拾取：槽位由客户端决定，服务端无需处理（仅消费载荷）
            let _ = icbm_mc_767::packets::play::serverbound::SPickItem::decode(reader)?;
        }
        play_ids::SERVERBOUND_PLAYER_ABILITIES => {
            // 创造/旁观飞行切换（双击空格）：解码消费即可，飞行状态由客户端掌控
            let _ = icbm_mc_767::packets::play::serverbound::SPlayerAbilities::decode(reader)?;
        }
        play_ids::SERVERBOUND_USE_ITEM_ON => {
            let packet = icbm_mc_767::packets::play::serverbound::SUseItemOn::decode(reader)?;
            handle_use_item_on(server, client, &packet)?;
        }
        play_ids::SERVERBOUND_USE_ITEM => {
            let _ = icbm_mc_767::packets::play::serverbound::SUseItem::decode(reader)?;
            handle_use_item_ender_pearl(server, client)?;
        }
        play_ids::SERVERBOUND_SET_CREATIVE_MODE_SLOT => {
            let packet =
                icbm_mc_767::packets::play::serverbound::SSetCreativeModeSlot::decode(reader)?;
            handle_set_creative_slot(client, &packet);
        }
        play_ids::SERVERBOUND_SET_CARRIED_ITEM => {
            // 选中热栏槽（0..8）：手持物品 = 热栏对应槽（若有物品）。
            // 旧实现直接丢弃该包，真实客户端（创造/生存）放置方块永远
            // 没有手持物品（红石无法激活根因）。
            let packet = icbm_mc_767::packets::play::serverbound::SSetCarriedItem::decode(reader)?;
            client.carried_slot.store(packet.slot, Ordering::Relaxed);
            let item = {
                let hotbar = client.hotbar.lock().expect("hotbar");
                hotbar
                    .get(usize::try_from(packet.slot).unwrap_or(usize::MAX))
                    .and_then(Clone::clone)
            };
            *client.held_item.lock().expect("held item") = item;
        }
        play_ids::SERVERBOUND_PLAYER_COMMAND => {
            let _ = icbm_mc_767::packets::play::serverbound::SPlayerCommand::decode(reader)?;
        }
        play_ids::SERVERBOUND_PLAYER_INPUT => {
            let _ = icbm_mc_767::packets::play::serverbound::SPlayerInput::decode(reader)?;
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
///
/// 主世界 Y 低于世界底部（-64）视为坠入虚空：本实现没有死亡/重生系统，直接
/// 把玩家传送回出生点地表（对照 vanilla 虚空伤害→重生到世界出生点），避免
/// 无限下坠。
#[allow(clippy::too_many_lines)] // 虚空救援 + region 迁移状态机分支
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

    // 虚空救援：主世界 Y < 世界底部（-64）→ 传回出生点（世界 spawn，地表上方）。
    // 区块加载期间坠入虚空（如初始区块批次未及时到达）也会在这里被拉回。
    if y < OVERWORLD_MIN_Y && client.dimension.load(Ordering::Relaxed) == 0 {
        let (sx, sy, sz) = {
            let spawn = server.world.lock().expect("world lock").spawn;
            (f64::from(spawn.x), f64::from(spawn.y), f64::from(spawn.z))
        };
        let (yaw, pitch) = {
            let mut position = client.position.lock().expect("position");
            position.x = sx;
            position.y = sy;
            position.z = sz;
            (position.yaw, position.pitch)
        };
        let teleport_id = client.next_teleport_id();
        client.send_encoded(CPlayerPosition::packet_id(), |w| {
            CPlayerPosition {
                x: sx,
                y: sy,
                z: sz,
                yaw,
                pitch,
                flags: 0,
                teleport_id,
            }
            .encode(w);
        })?;
        client.reset_move_baseline();
        debug!(name = %client.name.lock().expect("name"), y, "rescued from void below world, teleported to spawn");
        return Ok(());
    }

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
    // 敌对生物追踪：向所在 region 同步玩家位置（节流：约 4 次/秒）。
    // 虚拟区（Y>400）不关联 region，跳过。
    if let Some(region) = new_region
        && client
            .last_player_move_sent
            .lock()
            .expect("last move sent")
            .elapsed()
            >= Duration::from_millis(250)
    {
        let (px, py, pz) = {
            let position = client.position.lock().expect("position");
            (position.x, position.y, position.z)
        };
        let body = icbm_region::protocol::encode_player_move(&uuid, px, py, pz);
        server.ipc.send(
            &region.service_name(),
            icbm_region::protocol::kind::PLAYER_MOVE,
            &body,
        )?;
        *client.last_player_move_sent.lock().expect("last move sent") = Instant::now();
    }
    // 屏障拦截（节流：约 4 次/秒）：玩家位置在屏障方块内时回退到最近合法
    // 位置。乘船（走 MOVE_VEHICLE）时客户端不发送本函数，天然放行。
    if let Some(region) = new_region
        && client
            .last_barrier_check
            .lock()
            .expect("last barrier check")
            .elapsed()
            >= Duration::from_millis(250)
    {
        check_barrier_blocked(server, client, region, uuid)?;
        *client
            .last_barrier_check
            .lock()
            .expect("last barrier check") = Instant::now();
    }
    // 传送门检测（节流：约每 0.5 秒一次，避免每 tick 查询 region）
    check_portal_entry(server, client)?;
    Ok(())
}

/// 屏障拦截：查询玩家位置方块，命中屏障（10366）时回退到最近合法位置。
///
/// 对照需求：鞘翅/创造飞行不能快速穿过屏障；乘船（`MOVE_VEHICLE`）时不经过
/// 本检查（客户端在船上发 `VehicleMoveC2SPacket`），水面乘船自然放行。
fn check_barrier_blocked(
    server: &Server,
    client: &Arc<Client>,
    region: icbm_region::RegionId,
    uuid: uuid::Uuid,
) -> Result<(), NetError> {
    let (bx, by, bz) = {
        let position = client.position.lock().expect("position");
        (
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        )
    };
    client
        .barrier_query_pending
        .store(true, std::sync::atomic::Ordering::Relaxed);
    server.ipc.send(
        &region.service_name(),
        icbm_region::protocol::kind::BLOCK_QUERY,
        &icbm_region::protocol::encode_block_query(
            icbm_region::protocol::DIM_OVERWORLD,
            bx,
            by,
            bz,
        ),
    )?;
    // 等待响应（receiver 线程路由到 pending_barrier_state）
    let deadline = Instant::now() + Duration::from_millis(300);
    let state = loop {
        let value = client
            .pending_barrier_state
            .load(std::sync::atomic::Ordering::Relaxed);
        if value != -1 {
            break value;
        }
        if Instant::now() > deadline {
            break -1;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // 清标志与残留响应（含超时未消费的情况——否则迟到响应会路由错目标）
    client
        .pending_barrier_state
        .store(-1, std::sync::atomic::Ordering::Relaxed);
    client
        .barrier_query_pending
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if state != icbm_mc_767::packets::play::block_state_ids::BARRIER {
        // 位置合法：记录为回退基准
        *client.last_valid_position.lock().expect("last valid") =
            *client.position.lock().expect("position");
        return Ok(());
    }
    // 命中屏障：回退到最近合法位置
    let fallback = *client.last_valid_position.lock().expect("last valid");
    let teleport_id = client.next_teleport_id();
    let (x, y, z) = (fallback.x, fallback.y, fallback.z);
    client.send_encoded(CPlayerPosition::packet_id(), |w| {
        CPlayerPosition {
            x,
            y,
            z,
            yaw: fallback.yaw,
            pitch: fallback.pitch,
            flags: 0,
            teleport_id,
        }
        .encode(w);
    })?;
    // 同步服务端位置与 region 归属（复用 update_position 的迁移路径）
    update_position(server, client, |pos| {
        pos.x = x;
        pos.y = y;
        pos.z = z;
    })?;
    debug!(
        name = %client.name.lock().expect("name"),
        uuid = %uuid,
        bx, by, bz,
        "barrier blocked, teleported back"
    );
    client.reset_move_baseline();
    Ok(())
}

/// 新玩家出生点适配：查询世界出生点上方的真实地表高度并调整玩家 Y。
///
/// 超平坦自 Y=-64 铺设（地表 -61），但旧世界/被修改区块的地表可能不同——
/// 固定 -60 会“埋在地里”。经 `SURFACE_QUERY` 拿实际地表，站在其上。
pub fn adjust_spawn_to_surface(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    let (sx, sz) = {
        let spawn = server.world.lock().expect("world lock").spawn;
        (spawn.x, spawn.z)
    };
    let region = icbm_region::RegionId::from_overworld_block(sx, sz);
    server.ensure_region(region)?;
    client
        .surface_query_pending
        .store(true, std::sync::atomic::Ordering::Relaxed);
    server.ipc.send(
        &region.service_name(),
        icbm_region::protocol::kind::SURFACE_QUERY,
        &icbm_region::protocol::encode_surface_query(icbm_region::protocol::DIM_OVERWORLD, sx, sz),
    )?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let surface = loop {
        let value = client.pending_surface.lock().expect("surface").take();
        if let Some(value) = value {
            break value;
        }
        if Instant::now() > deadline {
            break -60; // 超时回退：新超平坦地表
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    client.pending_surface.lock().expect("surface").take();
    client
        .surface_query_pending
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let mut position = client.position.lock().expect("position");
    position.x = f64::from(sx) + 0.5;
    position.y = f64::from(surface);
    position.z = f64::from(sz) + 0.5;
    debug!(surface, "new player spawn adjusted to surface");
    Ok(())
}

/// 检测玩家是否站在下界传送门方块上；是则执行维度切换。
///
/// 查询玩家脚下方块（经 IPC 到所在 region）；传送门方块触发 1:8 缩放传送。
fn check_portal_entry(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    // 节流：每 10 tick（0.5 秒）查询一次
    if client
        .last_portal_check
        .lock()
        .expect("portal check")
        .elapsed()
        < Duration::from_millis(500)
    {
        return Ok(());
    }
    // 维度切换冷却（10 tick = 0.5s）：刚传送完成不立即重入
    if client
        .last_portal_switch
        .lock()
        .expect("portal switch")
        .elapsed()
        < Duration::from_millis(500)
    {
        return Ok(());
    }
    let uuid = *client.uuid.lock().expect("uuid");
    let in_region = server
        .world
        .lock()
        .expect("world lock")
        .player_regions
        .contains_key(&uuid);
    if !in_region {
        return Ok(()); // 虚拟区（Y>400）不检测
    }
    let dimension = client.dimension.load(Ordering::Relaxed);
    let (bx, by, bz) = {
        let position = client.position.lock().expect("position");
        (
            position.x.floor() as i32,
            position.y.floor() as i32,
            position.z.floor() as i32,
        )
    };
    // 按当前维度查询：主世界查主世界面；地狱查地狱面（地狱坐标 ≠ 主世界坐标，
    // 旧实现恒用主世界面导致地狱内的返回传送门永远检测不到）
    let (dim_byte, region_id) = if dimension == 1 {
        (
            icbm_region::protocol::DIM_NETHER,
            icbm_region::RegionId::from_nether_chunk(bx.div_euclid(16), bz.div_euclid(16)),
        )
    } else {
        (
            icbm_region::protocol::DIM_OVERWORLD,
            icbm_region::RegionId::from_overworld_chunk(bx.div_euclid(16), bz.div_euclid(16)),
        )
    };
    // 置传送门查询标志（receiver 据此把响应路由到 pending_portal_block，
    // 不与屏障检测的 pending_barrier_state 混淆）
    client
        .portal_query_pending
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // 只查询脚下方块一个探针：玩家站在传送门内部（2×3 开口）时脚下方块即为
    // 传送门。旧实现同时查脚下与脚下-1 两次，两次响应都匹配路由
    // `(py == y || py - 1 == y)`，与屏障检测（同样发 BLOCK_QUERY）并发时
    // 第一个响应可能被 `barrier_query_pending` 抢先消费 → 传送门查询超时
    // （“站入传送门不触发切换”）。单探针后每次查询恰好一个响应。
    server.ipc.send(
        &region_id.service_name(),
        icbm_region::protocol::kind::BLOCK_QUERY,
        &icbm_region::protocol::encode_block_query(dim_byte, bx, by, bz),
    )?;
    // 等待响应（receiver 线程处理 BLOCK_QUERY_RESP 后设置标记）
    let deadline = Instant::now() + Duration::from_millis(200);
    let mut found = false;
    while Instant::now() < deadline {
        let pending = client.pending_portal_block.load(Ordering::Relaxed);
        if pending != -1 {
            found = pending == 5864 || pending == 5865; // nether_portal axis=x/z
            client.pending_portal_block.store(-1, Ordering::Relaxed);
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if !found {
        *client.last_portal_check.lock().expect("portal check") = Instant::now();
        return Ok(());
    }
    // 触发维度切换：主世界 → 地狱（1:8 缩放），地狱 → 主世界
    let (nx, ny, nz) = if dimension == 0 {
        (bx as f64 / 8.0, by as f64, bz as f64 / 8.0)
    } else {
        (bx as f64 * 8.0, by as f64, bz as f64 * 8.0)
    };
    let target = u8::from(dimension == 0);
    let result = switch_dimension(server, client, target, nx, nz);
    *client.last_portal_check.lock().expect("portal check") = Instant::now();
    result
}

/// 玩家传送：设置新位置并处理跨 region 迁移（`/tp` 命令用）。
///
/// 复用 [`update_position`] 的迁移路径：先更新位置，再处理 region 归属变化、
/// 补视野区块；同时发 `CPlayerPosition` 同步客户端，并重置速度基准。
pub fn update_position_for_teleport(
    server: &Server,
    client: &Arc<Client>,
    x: f64,
    y: f64,
    z: f64,
) -> Result<(), NetError> {
    update_position(server, client, |pos| {
        pos.x = x;
        pos.y = y;
        pos.z = z;
    })?;
    // 同步客户端位置（绝对坐标、无标志位、新传送 ID）。yaw/pitch 先单次短锁
    // 取出，避免在发送闭包内再次锁 position（与 receiver 线程的转发竞争）。
    let teleport_id = client.next_teleport_id();
    let (yaw, pitch) = {
        let position = client.position.lock().expect("position");
        (position.yaw, position.pitch)
    };
    client.send_encoded(CPlayerPosition::packet_id(), |w| {
        CPlayerPosition {
            x,
            y,
            z,
            yaw,
            pitch,
            flags: 0,
            teleport_id,
        }
        .encode(w);
    })?;
    client.reset_move_baseline();
    Ok(())
}

/// 维度切换（下界传送门）：`CRespawn` + 更新位置 + 重请求视野区块。
///
/// 目标坐标按 1:8 缩放（主世界 ↔ 地狱）；`dimension` 为 0（主世界）或 1（地狱）。
/// 目的端 Y 固定到该维度地表（主世界超平坦地表 -60 / 地狱顶部基岩之上 128），
/// 并在落点生成一扇返回传送门（vanilla `PortalForcer.createPortal` 行为，
/// 对照 README“进入时无传送门生成/激活失败”修复）；玩家传送到传送门内部
/// 中心，走进即可返回原维度。
pub fn switch_dimension(
    server: &Server,
    client: &Arc<Client>,
    dimension: u8,
    x: f64,
    z: f64,
) -> Result<(), NetError> {
    use icbm_mc_767::packets::play::clientbound::{CRespawn, PlayerSpawnData};
    use icbm_mc_767::packets::play::dimension_names;
    use icbm_mc_767::packets::play::dimension_type_ids;
    use icbm_region::portal::{PortalAxis, portal_center};
    let current = client.dimension.load(Ordering::Relaxed);
    if current == dimension {
        return Ok(()); // 已在目标维度
    }
    // 目标 Y 固定到目标维度地表（superflat：主世界 -60 / 地狱 128），避免
    // 从另一维度返回时把源维度高度带过来（旧实现 `max` 保留源 Y，主世界
    // 返回会落在 y=128 空中）。
    let (x, y, z) = if dimension == 1 {
        (x, NETHER_SURFACE_FEET_Y, z)
    } else {
        (x, OVERWORLD_SURFACE_FEET_Y, z)
    };
    // 目的端生成返回传送门（fire-and-forget；region 在落点周边建 4×5 框架并
    // 填充传送门方块——没有返回传送门玩家将困在目的维度）
    let (region_id, dim_byte) = if dimension == 1 {
        (
            icbm_region::RegionId::from_nether_block(x as i32, z as i32),
            icbm_region::protocol::DIM_NETHER,
        )
    } else {
        (
            icbm_region::RegionId::from_overworld_block(x as i32, z as i32),
            icbm_region::protocol::DIM_OVERWORLD,
        )
    };
    server.ensure_region(region_id)?;
    let _ = server.ipc.send(
        &region_id.service_name(),
        icbm_region::protocol::kind::PORTAL_CREATE,
        &icbm_region::protocol::encode_portal_create(dim_byte, x as i32, y as i32, z as i32),
    );
    // 玩家传送到传送门内部中心（走进即可返回；中心与 region `create_portal`
    // 的轴选择一致——确定性奇偶公式）
    let axis = PortalAxis::from_pos(x as i32, z as i32);
    let (px, py, pz) = portal_center(x as i32, y as i32, z as i32, axis);
    let (x, y, z) = (f64::from(px), f64::from(py), f64::from(pz));
    // 更新服务端位置（先处理跨 region 迁移）
    update_position(server, client, |pos| {
        pos.x = x;
        pos.y = y;
        pos.z = z;
    })?;
    // 发送 CRespawn（维度切换）
    let spawn_data = PlayerSpawnData {
        dimension_type: if dimension == 1 {
            dimension_type_ids::THE_NETHER
        } else {
            dimension_type_ids::OVERWORLD
        },
        dimension_name: if dimension == 1 {
            dimension_names::THE_NETHER.to_owned()
        } else {
            dimension_names::OVERWORLD.to_owned()
        },
        hashed_seed: 0,
        game_mode: client.gamemode.lock().expect("gamemode").id(),
        previous_gamemode: -1,
        is_debug: false,
        is_flat: true,
        death_dimension: None,
        portal_cooldown: 10,
    };
    let respawn = CRespawn {
        spawn_data,
        data_kept: CRespawn::KEEP_ATTRIBUTES | CRespawn::KEEP_TRACKED_DATA,
    };
    client.send_encoded(CRespawn::packet_id(), |w| {
        respawn.encode(w);
    })?;
    client.dimension.store(dimension, Ordering::Relaxed);
    *client.last_portal_switch.lock().expect("portal switch") = Instant::now();
    // 位置同步（yaw/pitch 先单次短锁取出，避免发送闭包内重复锁 position——
    // 同一表达式两个临时 guard 会让 std Mutex 死锁，维度切换后连接线程挂死）
    let (yaw, pitch) = {
        let position = client.position.lock().expect("position");
        (position.yaw, position.pitch)
    };
    let teleport_id = client.next_teleport_id();
    client.send_encoded(CPlayerPosition::packet_id(), |w| {
        CPlayerPosition {
            x,
            y,
            z,
            yaw,
            pitch,
            flags: 0,
            teleport_id,
        }
        .encode(w);
    })?;
    // 重请求视野区块（地狱维度：请求地狱区块）
    request_dimension_chunks(server, client, dimension)?;
    client.reset_move_baseline();
    Ok(())
}

/// 请求当前维度的视野区块（维度切换后重发）。
fn request_dimension_chunks(
    server: &Server,
    client: &Arc<Client>,
    dimension: u8,
) -> Result<(), NetError> {
    let (chunk_x, chunk_z) = {
        let position = client.position.lock().expect("position");
        (position.chunk_x(), position.chunk_z())
    };
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
    // 地狱维度：请求所属 region 的地狱面区块
    if dimension == 1 {
        let region_id = icbm_region::RegionId::from_nether_chunk(chunk_x, chunk_z);
        server.ensure_region(region_id)?;
        server.ipc.send(
            &region_id.service_name(),
            icbm_region::protocol::kind::CHUNK_REQUEST,
            &icbm_region::protocol::encode_chunk_request(
                icbm_region::protocol::DIM_NETHER,
                chunk_x,
                chunk_z,
                radius,
            ),
        )?;
    } else {
        // 主世界：现有请求路径
        request_chunks_around(server, client, chunk_x, chunk_z)?;
    }
    Ok(())
}

/// 玩家动作：开始/完成破坏时把方块置为空气（发 `BLOCK_UPDATE`）。
fn handle_player_action(
    server: &Server,
    client: &Arc<Client>,
    packet: &icbm_mc_767::packets::play::serverbound::SPlayerAction,
) -> Result<(), NetError> {
    // status：0=开始破坏 1=取消 2=完成破坏（其余：掉落/使用物品等）
    if packet.status != 0 && packet.status != 2 {
        return Ok(());
    }
    let (bx, by, bz) = (packet.location.x, packet.location.y, packet.location.z);
    if f64::from(by) > VIRTUAL_ZONE_MIN_Y {
        return Ok(());
    }
    let region = RegionId::from_overworld_block(bx, bz);
    server.ensure_region(region)?;
    let body = icbm_region::protocol::encode_block_update(
        icbm_region::protocol::DIM_OVERWORLD,
        bx,
        by,
        bz,
        0, // 空气
    );
    server.ipc.send(
        &region.service_name(),
        icbm_region::protocol::kind::BLOCK_UPDATE,
        &body,
    )?;
    let _ = client;
    Ok(())
}

/// 玩家右键方块（放置方块）：从手持物品确定方块，发 `BLOCK_UPDATE` 到 region。
///
/// 放置位置 = 点击方块 + 面方向偏移（对照 vanilla `ItemUsageContext`/放置逻辑）。
fn handle_use_item_on(
    server: &Server,
    client: &Arc<Client>,
    packet: &icbm_mc_767::packets::play::serverbound::SUseItemOn,
) -> Result<(), NetError> {
    // 手持物品 → 可放置方块（默认无）
    let held = client.held_item.lock().expect("held item").clone();
    let Some(item) = held else {
        return Ok(()); // 空手：不放置
    };
    // 物品名 → 方块名（多数一致；红石粉物品注册键 `redstone` 对应方块
    // `redstone_wire`，对照 vanilla `BlockItem` 注册）
    let block = item_to_block_name(&item);
    // give 的物品名即方块名（stone/dirt 等）
    let state_id = icbm_mc_767::registry::name_to_state_id(&format!("minecraft:{block}"));
    let Some(state_id) = state_id else {
        return Ok(());
    };
    // 放置位置：点击方块 + 面方向
    let (dx, dy, dz) = face_offset(packet.face);
    let (bx, by, bz) = (
        packet.location.x + dx,
        packet.location.y + dy,
        packet.location.z + dz,
    );
    // Y>400 虚拟区不放置
    if f64::from(by) > VIRTUAL_ZONE_MIN_Y {
        return Ok(());
    }
    let region = RegionId::from_overworld_block(bx, bz);
    server.ensure_region(region)?;
    let body = icbm_region::protocol::encode_block_update(
        icbm_region::protocol::DIM_OVERWORLD,
        bx,
        by,
        bz,
        state_id,
    );
    server.ipc.send(
        &region.service_name(),
        icbm_region::protocol::kind::BLOCK_UPDATE,
        &body,
    )?;
    Ok(())
}

/// Direction 枚举序 → 偏移（0=下 1=上 2=北 3=南 4=西 5=东）。
#[must_use]
const fn face_offset(face: i32) -> (i32, i32, i32) {
    match face {
        0 => (0, -1, 0),
        1 => (0, 1, 0),
        2 => (0, 0, -1),
        3 => (0, 0, 1),
        4 => (-1, 0, 0),
        _ => (1, 0, 0),
    }
}

/// 物品名 → 可放置方块名（多数物品名即方块名；`redstone`（红石粉）对应
/// 方块 `redstone_wire`——物品与方块的注册键不同名，对照 vanilla `BlockItem`）。
/// 桶（`FluidItem`/`BucketItem`）放置的是对应流体方块：`water_bucket → water`、
/// `lava_bucket → lava`（`LiquidBlock` 默认态 = 源块，level 0）。
#[must_use]
fn item_to_block_name(item: &str) -> String {
    match item {
        "redstone" => "redstone_wire".to_owned(),
        "water_bucket" => "water".to_owned(),
        "lava_bucket" => "lava".to_owned(),
        other => other.to_owned(),
    }
}

/// 使用末影珍珠：从眼睛位置沿视线方向射线检测（region 计算落点），
/// 命中后直接传送（不加载区块、无实体投掷——对照需求“计算落点后直接传送”）。
fn handle_use_item_ender_pearl(server: &Server, client: &Arc<Client>) -> Result<(), NetError> {
    let held = client.held_item.lock().expect("held item").clone();
    tracing::info!("[pearl] held={held:?}");
    if held.as_deref() != Some("ender_pearl") {
        return Ok(());
    }
    let (origin, dir) = {
        let position = *client.position.lock().expect("position");
        // 眼睛高度 +1.62（vanilla `Entity.getEyeHeight` 站立）；角度换算
        let eye_y = position.y + 1.62;
        let yaw = f64::from(position.yaw).to_radians();
        let pitch = f64::from(position.pitch).to_radians();
        let dx = -yaw.sin() * pitch.cos();
        let dy = -pitch.sin();
        let dz = yaw.cos() * pitch.cos();
        ([position.x, eye_y, position.z], [dx, dy, dz])
    };
    let region = {
        let position = client.position.lock().expect("position");
        icbm_region::RegionId::from_overworld_chunk(position.chunk_x(), position.chunk_z())
    };
    server.ensure_region(region)?;
    client
        .raycast_pending
        .store(true, std::sync::atomic::Ordering::Relaxed);
    server.ipc.send(
        &region.service_name(),
        icbm_region::protocol::kind::BLOCK_RAYCAST,
        &icbm_region::protocol::encode_block_raycast(origin, dir, 120.0),
    )?;
    // 等待响应（receiver 线程写入 pending_raycast；负载高时 region 响应可能
    // 延迟，给足 3 秒避免测试/实际使用中误判未命中）
    let deadline = Instant::now() + Duration::from_secs(3);
    let hit = loop {
        let result = client.pending_raycast.lock().expect("raycast").take();
        if let Some(result) = result {
            break result;
        }
        if Instant::now() > deadline {
            client
                .raycast_pending
                .store(false, std::sync::atomic::Ordering::Relaxed);
            break (false, 0.0, 0.0, 0.0);
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let (hit, x, y, z) = hit;
    tracing::info!("[pearl] raycast hit={hit} ({x},{y},{z})");
    if !hit {
        return Ok(());
    }
    // 传送到命中方块上方 1 格（落点）
    debug!(
        name = %client.name.lock().expect("name"),
        x, y, z, "ender pearl teleport"
    );
    update_position_for_teleport(server, client, x, y + 1.0, z)?;
    Ok(())
}

/// 创造模式物品栏槽位更新：记录热栏（槽 36..44）物品为手持物品。
///
/// vanilla `SET_CREATIVE_MODE_SLOT` 的槽位是 `InventoryMenu` 槽号：
/// 0=合成结果、1..4=合成格、5..8=防具、9..35=主包、**36..44=热栏**、45=副手
/// （对照 `ServerGamePacketListenerImpl.handleSetCreativeModeSlot` 的
/// `inventoryMenu.getSlot(slotNum)` 与客户端 `handleCreativeModeItemAdd(stack, 36+j)`）。
/// 旧实现按槽 1..9 当热栏——真实客户端发送 36..44，热栏永不记录、`held_item`
/// 恒 None，**任何方块/桶都无法放置**（红石/黑曜石框架/水桶的共同根因）。
/// 热栏物品在 `SET_CARRIED_ITEM` 选中后作为手持物品。
fn handle_set_creative_slot(
    client: &Arc<Client>,
    packet: &icbm_mc_767::packets::play::serverbound::SSetCreativeModeSlot,
) {
    // 热栏：vanilla 槽 36..44（hotbar index 0..8）。槽 < 0 = 丢弃（光标，无槽位）。
    let hotbar_slot = match packet.slot {
        36..=44 => Some(packet.slot - 36),
        // 兼容旧客户端/直接写槽号（1..9）的记录
        1..=9 => Some(packet.slot - 1),
        _ => None,
    };
    if let Some(index) = hotbar_slot {
        let index = usize::try_from(index).unwrap_or(0);
        let item = packet.stack.item.clone();
        {
            let mut hotbar = client.hotbar.lock().expect("hotbar");
            if let Some(slot) = hotbar.get_mut(index) {
                *slot = item;
            }
        }
        if i32::from(index as i16) == client.carried_slot.load(Ordering::Relaxed) {
            let hotbar = client.hotbar.lock().expect("hotbar");
            (*client.held_item.lock().expect("held item"))
                .clone_from(&hotbar.get(index).and_then(Clone::clone));
        }
        return;
    }
    // 槽 0：兼容旧客户端/直接写槽 0 的记录（保留原行为）
    if packet.slot == 0 {
        (*client.held_item.lock().expect("held item")).clone_from(&packet.stack.item);
    }
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
                server.broadcast_gamerules();
            }
        }
        Some(Command::Help) => {
            let _ = client.send_system_chat(
                "可用命令: /about, /gamemode, /gamerule, /bmtool tps, /tp, /setblock, /fill, /give",
                false,
            );
        }
        Some(Command::About) => {
            let _ = client.send_system_chat(&about_message(), false);
        }
        Some(Command::Teleport { target, x, y, z }) => {
            let feedback = crate::console::apply_teleport(server, target.as_deref(), x, y, z);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::SetBlock { x, y, z, block }) => {
            let feedback = crate::console::apply_setblock(server, x, y, z, &block);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Fill { from, to, block }) => {
            let feedback = crate::console::apply_fill(server, from, to, &block);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Give {
            target,
            item,
            count,
        }) => {
            let feedback = crate::console::apply_give(server, &target, &item, count);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Ban { target, reason }) => {
            let feedback = crate::console::apply_ban(server, &target, reason.as_deref());
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Whitelist { action }) => {
            let feedback = crate::console::apply_whitelist(server, &action);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Enchant {
            target,
            enchantment,
            level,
        }) => {
            let feedback = crate::console::apply_enchant(server, &target, &enchantment, level);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Time { set, add }) => {
            if let Some(feedback) = crate::console::apply_time(server, set, add) {
                let _ = client.send_system_chat(&feedback, false);
            }
        }
        Some(Command::Weather { kind, duration }) => {
            let feedback = crate::console::apply_weather(server, kind, duration);
            let _ = client.send_system_chat(&feedback, false);
        }
        Some(Command::Data {
            action,
            x,
            y,
            z,
            path,
            nbt,
        }) => {
            let feedback = crate::console::apply_data(
                server,
                client,
                action,
                x,
                y,
                z,
                path.as_deref(),
                nbt.as_deref(),
            );
            let _ = client.send_system_chat(&feedback, false);
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
const TOP_LEVEL_COMMANDS: &[&str] = &[
    "about",
    "ban",
    "bmtool",
    "enchant",
    "fill",
    "gamemode",
    "gamerule",
    "give",
    "help",
    "setblock",
    "tp",
    "whitelist",
];

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
                children: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            },
            Node::Literal {
                name: "about".to_owned(),
                children: vec![],
            },
            Node::Literal {
                name: "ban".to_owned(),
                children: vec![13],
            },
            Node::Literal {
                name: "bmtool".to_owned(),
                children: vec![20],
            },
            Node::Literal {
                name: "enchant".to_owned(),
                children: vec![12],
            },
            Node::Literal {
                name: "fill".to_owned(),
                children: vec![14],
            },
            Node::Literal {
                name: "gamemode".to_owned(),
                children: vec![15],
            },
            Node::Literal {
                name: "gamerule".to_owned(),
                children: vec![16],
            },
            Node::Literal {
                name: "give".to_owned(),
                children: vec![17],
            },
            Node::Literal {
                name: "help".to_owned(),
                children: vec![],
            },
            Node::Literal {
                name: "setblock".to_owned(),
                children: vec![18],
            },
            Node::Literal {
                name: "tp".to_owned(),
                children: vec![19],
            },
            Node::Literal {
                name: "whitelist".to_owned(),
                children: vec![21],
            },
            Node::Argument {
                name: "target".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "from".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "mode".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "rule".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "item".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "pos".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "destination".to_owned(),
                children: vec![],
            },
            Node::Literal {
                name: "tps".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "enchantment".to_owned(),
                children: vec![],
            },
            Node::Argument {
                name: "action".to_owned(),
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
        [cmd, _] if matches!(*cmd, "gamemode" | "give" | "ban") => online_player_names(server),
        ["gamerule"] => icbm_gameplay_minecraft::gamerules::GAME_RULES
            .iter()
            .map(|rule| rule.name.to_owned())
            .collect(),
        ["gamerule", rule] => gamerule_value_suggestions(rule),
        ["bmtool"] => vec!["tps".to_owned()],
        ["whitelist"] => vec!["on", "off", "add", "remove", "list"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
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
#[must_use]
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
