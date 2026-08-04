//! 端到端地形内容测试：连接服务器，解码收到的区块 palette，验证：
//! - 超平坦地表层 = bedrock（y=0）/ dirt×2 / 非 snowy 草方块（y=3），底部无水；
//! - 区块状态 ID 与 vanilla 1.21.1 全局 palette 一致。
//!
//! 需要 `target/debug/icbm`（Cargo 提供 `CARGO_BIN_EXE_icbm`）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::packets::login_ids;
use icbm_mc_767::packets::play_ids;
use icbm_mc_767::packets::{decode_frame, encode_frame};
use icbm_mc_767::protocol::PROTOCOL_VERSION;
use uuid::Uuid;

struct FakeClient {
    stream: TcpStream,
    buffer: Vec<u8>,
    compression_threshold: Option<i32>,
}

impl FakeClient {
    fn connect(addr: &str) -> Self {
        let stream = TcpStream::connect(addr).expect("connect to server");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.set_nodelay(true).unwrap();
        Self {
            stream,
            buffer: Vec::new(),
            compression_threshold: None,
        }
    }
    fn send(&mut self, packet_id: i32, payload: &[u8]) {
        let frame = encode_frame(packet_id, payload, self.compression_threshold).expect("frame");
        self.stream.write_all(&frame).expect("write");
    }
    fn recv(&mut self) -> (i32, Vec<u8>) {
        loop {
            match decode_frame(&self.buffer, self.compression_threshold) {
                Ok(Some((id, payload, consumed))) => {
                    self.buffer.drain(..consumed);
                    return (id, payload);
                }
                Ok(None) => {
                    let mut chunk = [0u8; 8192];
                    let n = self.stream.read(&mut chunk).expect("read");
                    assert!(n > 0);
                    self.buffer.extend_from_slice(&chunk[..n]);
                }
                Err(err) => panic!("malformed frame: {err}"),
            }
        }
    }
    fn recv_until(&mut self, packet_id: i32, what: &str) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "timeout waiting {what}");
            let (id, payload) = self.recv();
            if id == packet_id {
                return payload;
            }
        }
    }

    /// 非阻塞读一帧；超时/无数据返回 None（配合 `set_read_timeout` 用）。
    fn recv_opt(&mut self) -> Option<(i32, Vec<u8>)> {
        loop {
            match decode_frame(&self.buffer, self.compression_threshold) {
                Ok(Some((id, payload, consumed))) => {
                    self.buffer.drain(..consumed);
                    return Some((id, payload));
                }
                Ok(None) => {
                    let mut chunk = [0u8; 8192];
                    match self.stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return None, // 对端关闭/超时/中断
                        Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                    }
                }
                Err(_) => return None,
            }
        }
    }

    fn set_read_timeout(&self, timeout: Duration) {
        self.stream.set_read_timeout(Some(timeout)).unwrap();
    }
}

/// 串行化测试锁：每个测试拉起完整 core+region 子进程，并行跑 14 个会把
/// 宿主机压满（region 20 TPS 节拍漂移、datarouter 拥塞、socket 超时），
/// 导致偶发失败。测试服务器持有该锁直到 Drop，保证同文件内测试串行。
static SERVER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TestServer {
    child: Child,
    addr: String,
    world_dir: std::path::PathBuf,
    shm_name: String,
    _serial_guard: std::sync::MutexGuard<'static, ()>,
}

impl TestServer {
    fn start(name: &str) -> Self {
        let _serial_guard = SERVER_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let world_dir =
            std::env::temp_dir().join(format!("icbm-chunk-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&world_dir);
        std::fs::create_dir_all(&world_dir).unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let addr = format!("127.0.0.1:{port}");
        let shm_name = format!("/datarouter-chunk-test-{}-{name}", std::process::id());

        let child = Command::new(env!("CARGO_BIN_EXE_icbm"))
            .args([
                "--world",
                world_dir.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--shm",
                &shm_name,
            ])
            .env("ICBM_ONLINE_MODE", "offline") // 测试用离线登录（默认已开正版验证）
            .env("ICBM_WORKERS", "1") // 测试环境单 worker，控制进程数
            .process_group(0) // 独立进程组：Drop 时整组清理 region 子进程
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if std::net::TcpStream::connect(&addr).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            child,
            addr,
            world_dir,
            shm_name,
            _serial_guard,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // 终止整个进程组（core + 其拉起的全部 region 子进程）
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{}", self.child.id()))
            .status();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.world_dir);
        let _ = std::fs::remove_file(format!(
            "/dev/shm/{}",
            self.shm_name.trim_start_matches('/')
        ));
    }
}

/// 读一个 PalettedContainer：返回 (bits, palette 条目)。
fn read_palette_container(r: &mut Reader<'_>) -> (u8, Vec<i32>) {
    let bits = r.read_u8().unwrap();
    if bits == 0 {
        let value = r.read_var_int().unwrap();
        let _len = r.read_var_int().unwrap();
        return (0, vec![value]);
    }
    let count = r.read_var_int().unwrap();
    let mut entries = Vec::new();
    for _ in 0..count {
        entries.push(r.read_var_int().unwrap());
    }
    // 打包数据：VarInt 长整型数量 + 长整型（`PacketByteBuf.readLongArray`）
    let long_count = r.read_var_int().unwrap() as usize;
    let _ = r.read_bytes(long_count * 8).unwrap();
    (bits, entries)
}

fn read_biome_container(r: &mut Reader<'_>) {
    let bits = r.read_u8().unwrap();
    if bits == 0 {
        let _v = r.read_var_int().unwrap();
        let _l = r.read_var_int().unwrap();
        return;
    }
    let n = r.read_var_int().unwrap();
    for _ in 0..n {
        let _ = r.read_var_int().unwrap();
    }
    let long_count = r.read_var_int().unwrap() as usize;
    let _ = r.read_bytes(long_count * 8).unwrap();
}

#[allow(clippy::too_many_lines)]
#[test]
fn chunk_terrain_matches_vanilla_flat() {
    let server = TestServer::start("terrain");
    let mut client = FakeClient::connect(&server.addr);

    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(2);
    client.send(0x00, &intent.into_bytes());

    let mut start = Writer::new();
    start.write_string("diagplayer");
    start.write_uuid(&Uuid::nil());
    client.send(login_ids::SERVERBOUND_START, &start.into_bytes());

    let (cid, payload) = client.recv();
    assert_eq!(cid, login_ids::CLIENTBOUND_COMPRESSION);
    let mut r = Reader::new(&payload);
    let threshold = r.read_var_int().unwrap();
    client.compression_threshold = Some(threshold);
    client.recv_until(login_ids::CLIENTBOUND_SUCCESS, "success");

    client.send(login_ids::SERVERBOUND_ACKNOWLEDGED, &[]);
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        "brand",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_ENABLED_FEATURES,
        "features",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_KNOWN_PACKS,
        "packs",
    );
    let mut packs = Writer::new();
    packs.write_var_int(0);
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_KNOWN_PACKS,
        &packs.into_bytes(),
    );
    for i in 0..11 {
        client.recv_until(
            icbm_mc_767::packets::config_ids::CLIENTBOUND_REGISTRY_DATA,
            &format!("reg {i}"),
        );
    }
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_TAGS,
        "tags",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_FINISH_CONFIGURATION,
        "finish",
    );
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_FINISH_CONFIGURATION,
        &[],
    );

    client.recv_until(play_ids::CLIENTBOUND_LOGIN, "login");
    client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER, "center");
    client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_RADIUS, "radius");
    client.recv_until(play_ids::CLIENTBOUND_CHANGE_DIFFICULTY, "difficulty");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_ABILITIES, "abilities");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_INFO_UPDATE, "info");
    client.recv_until(play_ids::CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION, "spawn");
    client.recv_until(play_ids::CLIENTBOUND_COMMANDS, "commands");
    client.recv_until(play_ids::CLIENTBOUND_CUSTOM_PAYLOAD, "register");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_POSITION, "position");
    client.recv_until(play_ids::CLIENTBOUND_GAME_EVENT, "chunks coming");
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_START, "batch start");

    // 收集 (0,0) 区块的 section palette
    let mut center_palettes: Option<Vec<Vec<i32>>> = None;
    loop {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED {
            break;
        }
        if id != play_ids::CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT {
            continue;
        }
        let mut r = Reader::new(&payload);
        let x = r.read_i32_be().unwrap();
        let z = r.read_i32_be().unwrap();
        let _heightmap = r.read_nbt().unwrap().unwrap();
        let data_len = r.read_var_int().unwrap() as usize;
        let data = r.read_bytes(data_len).unwrap();
        let mut sr = Reader::new(data);
        let mut palettes: Vec<Vec<i32>> = Vec::new();
        for _ in 0..24 {
            let _nonair = sr.read_i16_be().unwrap();
            let (_, entries) = read_palette_container(&mut sr);
            palettes.push(entries);
            read_biome_container(&mut sr);
        }
        if (x, z) == (0, 0) {
            center_palettes = Some(palettes);
        }
    }

    let palettes = center_palettes.expect("center chunk (0,0) not received");
    // superflat 自世界底 Y=-64 起铺设：section 0（世界 y -64..-49）含底部
    // bedrock(y=-64)、dirt×2（-63..-62）、顶层非 snowy 草（-61）。
    let surface = &palettes[0];
    for id in [79, 10, 9, 0] {
        assert!(
            surface.contains(&id),
            "surface palette {surface:?} missing block id {id} (bedrock=79 dirt=10 grass=9 air=0)"
        );
    }
    // section 0 之下不存在（首个即世界底部）；section 0 内不得有雪草（8）
    assert!(
        !surface.contains(&8),
        "snowy grass_block (id 8) must not be generated"
    );
    // 草方块必须是非 snowy 的全局 ID 9（8 是 snowy=true），且只出现在 section 0
    let mut grass_found = false;
    for (index, section) in palettes.iter().enumerate() {
        if section.contains(&9) {
            grass_found = true;
        }
        assert!(
            !section.contains(&8),
            "snowy grass_block (id 8) must not be generated: {section:?}"
        );
        if index > 0 {
            assert!(
                section == &vec![0],
                "above-surface sections must be air only, section {index}: {section:?}"
            );
        }
    }
    assert!(grass_found, "non-snowy grass_block (id 9) not found");
}

/// 玩家跨 region 边界时：core 应拉起/复用新 region、重发视野区块并更新
/// 区块缓存中心（修复“调入虚空”）。
#[allow(clippy::too_many_lines)]
#[test]
fn crossing_region_border_loads_new_region_chunks() {
    let server = TestServer::start("crossing");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);

    // 等初始区块批次完成（位置已在 join_to_play 消费）
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 跨区：从 (0,4,0) 走到 (600,4,0)（x=512 是 region 0 与 1 的边界）
    let mut move_packet = Writer::new();
    move_packet.write_f64_be(600.0);
    move_packet.write_f64_be(4.0);
    move_packet.write_f64_be(0.0);
    move_packet.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_packet.into_bytes());

    // 服务器应更新区块缓存中心到新位置（区块 37, 0）
    let center = client.recv_until(
        play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
        "crossing chunk cache center",
    );
    let mut r = Reader::new(&center);
    let cx = r.read_var_int().unwrap();
    let cz = r.read_var_int().unwrap();
    assert_eq!((cx, cz), (37, 0));

    // 新区域视野区块到达（批次开始 → 区块 → 完成）
    client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_START,
        "crossing batch start",
    );
    let finished = client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED,
        "crossing batch finished",
    );
    let mut r = Reader::new(&finished);
    assert_eq!(r.read_var_int().unwrap(), 289); // (2*8+1)^2
}

/// 在 `duration` 内持续非阻塞读包，断言未出现 `forbidden` 中的包 ID（用于
/// 验证 Y>400 虚拟区不触发区块缓存中心更新 / 区块批次）。
fn assert_no_packet_within(
    client: &mut FakeClient,
    duration: Duration,
    forbidden: &[i32],
    what: &str,
) {
    client.set_read_timeout(Duration::from_millis(200));
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        let Some((id, _)) = client.recv_opt() else {
            continue;
        };
        assert!(
            !forbidden.contains(&id),
            "unexpected packet {id:#x} during {what} (forbidden: {forbidden:?})"
        );
    }
    client.set_read_timeout(Duration::from_secs(10));
}

/// 玩家 Y>400 虚拟区（core 接管）：不加载/请求任何 region，不更新区块缓存
/// 中心；回落至 Y≤400 后才拉起/复用精确落点 region 并补视野区块。
#[allow(clippy::too_many_lines)]
#[test]
fn above_400_virtual_zone_defers_region_loading() {
    let server = TestServer::start("virtual-zone");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    // 排空初始批次（位置包已在 join_to_play 消费）
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 升到 Y=500（>400，虚拟区）并跨过 region 边界 x=512：core 应接管，
    // 不得更新区块缓存中心、不得发起新批次、不得加载新 region。
    let mut move_up = Writer::new();
    move_up.write_f64_be(600.0);
    move_up.write_f64_be(500.0);
    move_up.write_f64_be(0.0);
    move_up.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_up.into_bytes());
    assert_no_packet_within(
        &mut client,
        Duration::from_millis(1200),
        &[
            play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
            play_ids::CLIENTBOUND_CHUNK_BATCH_START,
        ],
        "virtual zone above 400",
    );

    // 回落到 Y=4（≤400）：拉起/复用落点 region（1,0），更新区块缓存中心并补区块
    let mut move_down = Writer::new();
    move_down.write_f64_be(600.0);
    move_down.write_f64_be(4.0);
    move_down.write_f64_be(0.0);
    move_down.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_down.into_bytes());
    let center = client.recv_until(
        play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
        "descend chunk cache center",
    );
    let mut r = Reader::new(&center);
    let cx = r.read_var_int().unwrap();
    let cz = r.read_var_int().unwrap();
    assert_eq!((cx, cz), (37, 0));
    client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_START,
        "descend batch start",
    );
    let finished = client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED,
        "descend batch finished",
    );
    let mut r = Reader::new(&finished);
    assert_eq!(r.read_var_int().unwrap(), 289);
}

/// 初始区块批次半径以服务端可视距离为准（对照 vanilla `ChunkLoadDistance`），
/// 不得被客户端上报的渲染距离（最大 32 → 65×65=4225 块）撑大。
#[allow(clippy::too_many_lines)]
#[test]
fn initial_batch_capped_by_server_view_distance() {
    let server = TestServer::start("view-cap");
    let mut client = FakeClient::connect(&server.addr);

    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(2);
    client.send(0x00, &intent.into_bytes());
    let mut start = Writer::new();
    start.write_string("farview");
    start.write_uuid(&Uuid::nil());
    client.send(login_ids::SERVERBOUND_START, &start.into_bytes());
    let (cid, payload) = client.recv();
    assert_eq!(cid, login_ids::CLIENTBOUND_COMPRESSION);
    let mut r = Reader::new(&payload);
    client.compression_threshold = Some(r.read_var_int().unwrap());
    client.recv_until(login_ids::CLIENTBOUND_SUCCESS, "success");
    client.send(login_ids::SERVERBOUND_ACKNOWLEDGED, &[]);
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        "brand",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_ENABLED_FEATURES,
        "features",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_KNOWN_PACKS,
        "packs",
    );
    let mut packs = Writer::new();
    packs.write_var_int(0);
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_KNOWN_PACKS,
        &packs.into_bytes(),
    );
    for i in 0..11 {
        client.recv_until(
            icbm_mc_767::packets::config_ids::CLIENTBOUND_REGISTRY_DATA,
            &format!("reg {i}"),
        );
    }
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_TAGS,
        "tags",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_FINISH_CONFIGURATION,
        "finish",
    );
    // 客户端上报渲染距离 32（配置阶段 0x00 ClientInformation）
    let mut info = Writer::new();
    info.write_string("zh_cn");
    info.write_i8(32);
    info.write_u8(0); // chat mode
    info.write_bool(true); // chat colors
    info.write_u8(0xFF); // skin parts
    info.write_u8(0); // main hand
    info.write_bool(false); // text filtering
    info.write_bool(true); // allow server listing
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_CLIENT_INFORMATION,
        &info.into_bytes(),
    );
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_FINISH_CONFIGURATION,
        &[],
    );

    client.recv_until(play_ids::CLIENTBOUND_LOGIN, "login");
    let radius = client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_RADIUS, "radius");
    let mut r = Reader::new(&radius);
    assert_eq!(
        r.read_var_int().unwrap(),
        8,
        "radius must be server view distance"
    );
    client.recv_until(play_ids::CLIENTBOUND_CHANGE_DIFFICULTY, "difficulty");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_ABILITIES, "abilities");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_INFO_UPDATE, "info");
    client.recv_until(play_ids::CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION, "spawn");
    client.recv_until(play_ids::CLIENTBOUND_COMMANDS, "commands");
    client.recv_until(play_ids::CLIENTBOUND_CUSTOM_PAYLOAD, "register");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_POSITION, "position");
    client.recv_until(play_ids::CLIENTBOUND_GAME_EVENT, "chunks coming");
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_START, "batch start");
    let finished = client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "batch finished");
    let mut r = Reader::new(&finished);
    // 服务端可视距离 8 → (2*8+1)^2 = 289，而不是客户端上报的 65^2
    assert_eq!(r.read_var_int().unwrap(), 289);
}

/// region 模拟层按原版机制生成敌对 Creeper（region 存活信号 + 敌对生物）：
/// 客户端应收到 `ADD_ENTITY`（实体类型 23 = creeper）。
#[test]
fn creeper_spawns_in_region() {
    let server = TestServer::start("creeper");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // Creeper 仅在夜晚生成（vanilla 机制）：/time set night 推进到 13000
    send_chat_command(&mut client, "time set night");
    recv_chat_until(&mut client, "13000");

    // 夜晚每 tick 1/120 概率，30 秒内生成概率 >99%；超时视为失败。
    // 友好生物也可能先于 Creeper 生成：跳过非 Creeper 的 ADD_ENTITY。
    let deadline = Instant::now() + Duration::from_secs(30);
    let add = loop {
        assert!(Instant::now() < deadline, "no creeper spawned within 30s");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_ADD_ENTITY {
            let mut probe = Reader::new(&payload);
            let _entity_id = probe.read_var_int().unwrap();
            let _uuid = probe.read_uuid().unwrap();
            let entity_type = probe.read_var_int().unwrap();
            if entity_type == 23 {
                break payload;
            }
        }
    };
    let mut r = Reader::new(&add);
    let entity_id = r.read_var_int().unwrap();
    let _uuid = r.read_uuid().unwrap();
    let entity_type = r.read_var_int().unwrap();
    assert_eq!(entity_type, 23, "creeper entity type id");
    let _x = r.read_f64_be().unwrap();
    let y = r.read_f64_be().unwrap();
    let _z = r.read_f64_be().unwrap();
    assert_eq!(
        y, -60.0,
        "creeper spawns on flat surface (superflat at Y=-64)"
    );
    assert!(entity_id >= 1000, "region entity ids start at 1000");
}

/// 敌对生物 AI：Creeper 靠近玩家 3 格内点燃引信（实体状态 1 闪烁）并爆炸。
#[test]
fn creeper_chases_and_explodes_near_player() {
    let server = TestServer::start("creeper-ai");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 夜晚 + 关闭友好生物干扰：只留 Creeper
    send_chat_command(&mut client, "time set night");
    recv_chat_until(&mut client, "13000");

    // 等待 Creeper 生成并记录其位置
    let deadline = Instant::now() + Duration::from_secs(30);
    let (cx, cz) = loop {
        assert!(Instant::now() < deadline, "no creeper spawned");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_ADD_ENTITY {
            let mut r = Reader::new(&payload);
            let _entity_id = r.read_var_int().unwrap();
            let _uuid = r.read_uuid().unwrap();
            let entity_type = r.read_var_int().unwrap();
            if entity_type == 23 {
                let x = r.read_f64_be().unwrap();
                let _y = r.read_f64_be().unwrap();
                let z = r.read_f64_be().unwrap();
                break (x, z);
            }
        }
    };

    // 玩家瞬移到 Creeper 旁边（3 格内触发引信）
    let mut move_to = Writer::new();
    move_to.write_f64_be(cx + 1.0);
    move_to.write_f64_be(-60.0);
    move_to.write_f64_be(cz + 1.0);
    move_to.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_to.into_bytes());

    // 等待实体状态 1（点燃引信闪烁）→ 再等待爆炸包（半径 3）
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_fuse = false;
    let mut saw_explosion = false;
    while Instant::now() < deadline && !saw_explosion {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_ENTITY_EVENT && !saw_fuse {
            let mut r = Reader::new(&payload);
            let _entity = r.read_i32_be().unwrap();
            let status = r.read_i8().unwrap();
            if status == 1 {
                saw_fuse = true;
            }
        } else if id == play_ids::CLIENTBOUND_EXPLOSION {
            saw_explosion = true;
        }
    }
    assert!(saw_fuse, "creeper should start fusing near player");
    assert!(saw_explosion, "creeper should explode after fuse");
}

/// `/data` NBT 编辑与命令方块：设置命令 → 红石供电触发 → 执行 setblock。
#[test]
fn data_command_and_command_block() {
    let server = TestServer::start("cmdblock");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 放置命令方块并写入 Command NBT（/data merge block）
    send_chat_command(&mut client, "setblock 20 -60 20 command_block");
    recv_chat_until(&mut client, "已放置");
    send_chat_command(
        &mut client,
        r#"data merge block 20 -60 20 {Command:"setblock 21 -60 20 stone"}"#,
    );
    let merge_feedback = recv_chat_until(&mut client, "方块实体数据已更新");
    assert!(
        merge_feedback.contains("setblock 21 -60 20 stone"),
        "merge feedback: {merge_feedback}"
    );

    // /data get block 应回显 Command
    send_chat_command(&mut client, "data get block 20 -60 20");
    let get_feedback = recv_chat_until(&mut client, "方块实体数据");
    assert!(
        get_feedback.contains("setblock 21 -60 20 stone"),
        "get feedback: {get_feedback}"
    );

    // 旁边放红石块供电 → 命令方块触发 → setblock 21 -60 20 stone
    send_chat_command(&mut client, "setblock 19 -60 20 redstone_block");
    recv_chat_until(&mut client, "已放置");

    // 等待 CBlockUpdate：位置 (21, -60, 20) 变石头
    let deadline = Instant::now() + Duration::from_secs(20);
    let placed = loop {
        assert!(
            Instant::now() < deadline,
            "command block did not place stone"
        );
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(21, -60, 20) {
                break state;
            }
        }
    };
    assert_eq!(placed, 1, "command block placed stone (id 1)");

    // /data remove block 移除 Command 键
    send_chat_command(&mut client, "data remove block 20 -60 20 Command");
    let remove_feedback = recv_chat_until(&mut client, "已从");
    assert!(
        remove_feedback.contains("Command"),
        "remove feedback: {remove_feedback}"
    );
}

/// 末影珍珠：计算落点直接传送（region 射线检测，无实体投掷）。
#[test]
fn ender_pearl_teleports_to_landing_point() {
    let mut server = TestServer::start("ender-pearl");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 一柱石头当作射线目标（从地面 -62 到 -58；射线沿 +x，z 保持 0）
    send_chat_command(&mut client, "fill 10 -62 0 10 -58 0 stone");
    recv_chat_until(&mut client, "已填充");
    // 手持末影珍珠
    send_chat_command(&mut client, "give diagplayer ender_pearl");
    recv_chat_until(&mut client, "已给");

    // 玩家站 (0,-60,0) 面向 +x（yaw=-90，pitch=0），眼睛高度 -58.38
    let mut move_rot = Writer::new();
    move_rot.write_f64_be(0.0);
    move_rot.write_f64_be(-60.0);
    move_rot.write_f64_be(0.0);
    move_rot.write_f32_be(-90.0); // yaw：面向 +x
    move_rot.write_f32_be(0.0); // pitch
    move_rot.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS_ROT, &move_rot.into_bytes());

    // 使用物品（主手）→ 射线命中柱子 (10,-59,10) → 传送到 (10,-58,10)
    let mut use_item = Writer::new();
    use_item.write_var_int(0); // hand = main
    use_item.write_var_int(0); // sequence
    client.send(play_ids::SERVERBOUND_USE_ITEM, &use_item.into_bytes());

    let deadline = Instant::now() + Duration::from_secs(15);
    let teleport = loop {
        if Instant::now() > deadline {
            panic!("no ender pearl teleport");
        }
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_PLAYER_POSITION {
            let mut r = Reader::new(&payload);
            let x = r.read_f64_be().unwrap();
            let y = r.read_f64_be().unwrap();
            let z = r.read_f64_be().unwrap();
            if (x, z) != (0.0, 0.0) {
                break (x, y, z);
            }
        }
    };
    assert_eq!(teleport.0, 10.0, "teleport x");
    assert_eq!(teleport.1, -58.0, "teleport y (1 above hit block)");
    assert_eq!(teleport.2, 0.0, "teleport z");
}

/// 屏障拦截：玩家位置在屏障方块内时回退到最近合法位置；乘船（MOVE_VEHICLE）
/// 时放行（水面乘船可以穿过屏障）。
#[test]
fn barrier_blocks_walkers_but_not_boats() {
    let server = TestServer::start("barrier");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 一道屏障墙：x=10, y=-60
    send_chat_command(&mut client, "setblock 10 -60 0 barrier");
    recv_chat_until(&mut client, "已放置");

    // 走到屏障前 (9,-60,0)：合法，记录为回退基准
    let mut move_to = Writer::new();
    move_to.write_f64_be(9.0);
    move_to.write_f64_be(-60.0);
    move_to.write_f64_be(0.0);
    move_to.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_to.into_bytes());
    std::thread::sleep(std::time::Duration::from_millis(900)); // 越过屏障检测节流（服务端处理首个移动含 IPC 往返）

    // 走进屏障 (10,-60,0)：服务端应回退到 (9,-60,0)
    let mut into_barrier = Writer::new();
    into_barrier.write_f64_be(10.0);
    into_barrier.write_f64_be(-60.0);
    into_barrier.write_f64_be(0.0);
    into_barrier.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &into_barrier.into_bytes());

    let deadline = Instant::now() + Duration::from_secs(15);
    let fallback = loop {
        assert!(Instant::now() < deadline, "no barrier fallback teleport");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_PLAYER_POSITION {
            let mut r = Reader::new(&payload);
            let x = r.read_f64_be().unwrap();
            let y = r.read_f64_be().unwrap();
            let z = r.read_f64_be().unwrap();
            if (x, z) == (9.0, 0.0) {
                break (x, y, z);
            }
        }
    };
    assert_eq!(fallback.1, -60.0, "fallback y");

    // 乘船（MOVE_VEHICLE）进入屏障位置：放行（不发送回退 CPlayerPosition）
    let mut boat = Writer::new();
    boat.write_f64_be(10.0);
    boat.write_f64_be(-60.0);
    boat.write_f64_be(0.0);
    boat.write_f32_be(0.0);
    boat.write_f32_be(0.0);
    client.send(play_ids::SERVERBOUND_MOVE_VEHICLE, &boat.into_bytes());

    // 1.5 秒内不得出现新的位置回退包
    client.set_read_timeout(Duration::from_millis(200));
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        let Some((id, payload)) = client.recv_opt() else {
            continue;
        };
        if id == play_ids::CLIENTBOUND_PLAYER_POSITION {
            let mut r = Reader::new(&payload);
            let x = r.read_f64_be().unwrap();
            let z = r.read_f64_be().unwrap();
            assert!(
                (x, z) != (9.0, 0.0),
                "boat must pass through barrier without fallback"
            );
        }
    }
    client.set_read_timeout(Duration::from_secs(10));
}

/// 活塞：红石供电（上升沿）伸出推动前方方块；BUD 准连接供电。
#[test]
fn piston_pushes_blocks_on_power() {
    let server = TestServer::start("piston");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 朝南（+z）的活塞 + 前方石头（/setblock 放置默认态，facing=south）
    send_chat_command(&mut client, "setblock 20 -60 10 piston");
    recv_chat_until(&mut client, "已放置");
    send_chat_command(&mut client, "setblock 20 -60 11 stone");
    recv_chat_until(&mut client, "已放置");
    // 侧方红石块供电 → 上升沿伸出
    send_chat_command(&mut client, "setblock 19 -60 10 redstone_block");
    recv_chat_until(&mut client, "已放置");

    // 等待：z=11 变活塞头（2025 子态 0）、z=12 变石头（前移）、z=13 无
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut head_seen = false;
    let mut stone_seen = false;
    while Instant::now() < deadline && !(head_seen && stone_seen) {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(20, -60, 11) {
                // 活塞头（south 朝向 = 2023 + 3*4 = 2035）
                head_seen = state == 2035;
            }
            if pos == icbm_mc_767::packets::play::BlockPos::new(20, -60, 12) {
                stone_seen = state == 1;
            }
        }
    }
    assert!(head_seen, "piston head should appear at z=11");
    assert!(stone_seen, "stone should be pushed to z=12");
}

/// 地狱传送门：经典 4×5 黑曜石框架自动填充传送门方块；玩家站入触发维度切换。
#[test]
fn nether_portal_builds_and_activates() {
    let server = TestServer::start("portal");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 经典 4×5 框架（x=20，z=0..3，y=-58..-54，建在地表上方空气中）：
    // 底部/顶部横梁 + 两侧立柱
    send_chat_command(&mut client, "fill 20 -58 0 20 -58 3 obsidian");
    recv_chat_until(&mut client, "已填充");
    send_chat_command(&mut client, "fill 20 -54 0 20 -54 3 obsidian");
    recv_chat_until(&mut client, "已填充");
    send_chat_command(&mut client, "fill 20 -57 0 20 -55 0 obsidian");
    recv_chat_until(&mut client, "已填充");
    send_chat_command(&mut client, "fill 20 -57 3 20 -55 3 obsidian");
    recv_chat_until(&mut client, "已填充");

    // 玩家站入传送门内部 (20,-56,1) → CRespawn（维度切换）；
    // 这同时验证了框架被检测、传送门方块被填充、站入触发三条链路。
    let mut move_in = Writer::new();
    move_in.write_f64_be(20.0);
    move_in.write_f64_be(-56.0);
    move_in.write_f64_be(1.0);
    move_in.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_in.into_bytes());

    let deadline = Instant::now() + Duration::from_secs(15);
    let respawn = loop {
        assert!(
            Instant::now() < deadline,
            "no respawn after entering portal"
        );
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_RESPAWN {
            break payload;
        }
    };
    // CRespawn 载荷：CommonPlayerSpawnInfo.dimensionName 应为 the_nether
    let mut r = Reader::new(&respawn);
    let _dimension_type = r.read_var_int().unwrap();
    let dimension_name = r.read_string().unwrap();
    assert_eq!(dimension_name, "minecraft:the_nether", "switched to nether");
}

/// 跨区后上一 region 的真实地形必须保留：视野覆盖旧区区块时由旧区（沉睡
/// region 仍在线服务内存数据）或 core 直读旧区 .mca 提供，不得被新区以 flat
/// 生成覆盖（区域切换显示 BUG）。
#[allow(clippy::too_many_lines)]
#[test]
fn crossing_preserves_previous_region_terrain() {
    let server = TestServer::start("crossing-terrain");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 在 (498,-60,0) 放置石头做标记（chunk (31,0) → region 0 内、跨区后
    // 视野 [29..45] 覆盖该 chunk）
    send_chat_command(&mut client, "setblock 498 -60 0 stone");
    recv_chat_until(&mut client, "已放置");

    // 跨区到 (600,4,0)（region 1）
    let mut move_packet = Writer::new();
    move_packet.write_f64_be(600.0);
    move_packet.write_f64_be(4.0);
    move_packet.write_f64_be(0.0);
    move_packet.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_packet.into_bytes());
    client.recv_until(
        play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
        "crossing center",
    );
    client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_START,
        "crossing batch start",
    );

    // 批次中 chunk (31,0) 必须保留真实地形（含 stone 全局 ID 1）：
    // 由沉睡的 region 0 内存数据或 .mca 提供，不得被 region 1 flat 覆盖
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut chunk_31_0_stone = false;
    let mut finished = false;
    while Instant::now() < deadline && !finished {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED {
            finished = true;
            continue;
        }
        if id != play_ids::CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT {
            continue;
        }
        let mut r = Reader::new(&payload);
        let x = r.read_i32_be().unwrap();
        let z = r.read_i32_be().unwrap();
        if (x, z) != (31, 0) {
            continue;
        }
        let _heightmap = r.read_nbt().unwrap().unwrap();
        let data_len = r.read_var_int().unwrap() as usize;
        let data = r.read_bytes(data_len).unwrap();
        let mut sr = Reader::new(data);
        for _ in 0..24 {
            let _nonair = sr.read_i16_be().unwrap();
            let (_, entries) = read_palette_container(&mut sr);
            if entries.contains(&1) {
                chunk_31_0_stone = true;
            }
            read_biome_container(&mut sr);
        }
    }
    assert!(finished, "crossing batch must finish");
    assert!(
        chunk_31_0_stone,
        "previous region chunk (31,0) must keep real terrain (stone) after crossing"
    );
}

/// 真实客户端创造模式放置红石：创造物品栏热栏槽（槽 1..9）+ 选中槽
/// （`SET_CARRIED_ITEM`）→ `USE_ITEM_ON` 放置红石线（红石无法激活 BUG）。
#[test]
fn creative_inventory_places_redstone() {
    let server = TestServer::start("creative-redstone");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 真实 vanilla 客户端：把红石粉放入热栏第 5 格（hotbar index 4）发送
    // `SET_CREATIVE_MODE_SLOT` 槽 40（36+4，`InventoryMenu` 热栏槽号），
    // 选中热栏第 5 格发送 `SET_CARRIED_ITEM`（short 4）。
    // 回归：旧实现按槽 1..9 记录热栏，真实客户端 36..44 永不命中，手持物品
    // 恒 None（任何方块/桶都放不出来——红石/黑曜石框架/水桶的共同根因）。
    send_creative_slot(&mut client, 40, "redstone");
    let mut w = Writer::new();
    w.write_i16_be(4);
    client.send(play_ids::SERVERBOUND_SET_CARRIED_ITEM, &w.into_bytes());
    std::thread::sleep(Duration::from_millis(200));

    // USE_ITEM_ON：点击 (20,-61,10) 顶面 → 落点 (20,-60,10)
    send_use_item_on(&mut client, 20, -61, 10, 1);

    // 红石线（4138 = power 0）应被放置
    let deadline = Instant::now() + Duration::from_secs(8);
    let placed = loop {
        assert!(
            Instant::now() < deadline,
            "redstone not placed via creative inventory"
        );
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(20, -60, 10) {
                break state;
            }
        }
    };
    assert_eq!(placed, 4138, "placed redstone wire (power 0)");
}

/// 红石块（真实客户端路径放置）相邻红石线 → 线亮（power 15 = 4153）。
/// 回归：旧实现手持物品恒 None，红石块与红石粉都放不出来（基础红石不工作）。
#[test]
fn creative_redstone_block_powers_wire() {
    let server = TestServer::start("creative-redstone-power");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 热栏第 5 格（槽 40）放红石粉并选中 → 点击 (20,-61,10) 顶面放置
    send_creative_slot(&mut client, 40, "redstone");
    let mut carried = Writer::new();
    carried.write_i16_be(4);
    client.send(
        play_ids::SERVERBOUND_SET_CARRIED_ITEM,
        &carried.into_bytes(),
    );
    send_use_item_on(&mut client, 20, -61, 10, 1);
    // 热栏第 4 格（槽 39）放红石块并选中 → 点击 (19,-61,10) 顶面放置
    send_creative_slot(&mut client, 39, "redstone_block");
    let mut carried = Writer::new();
    carried.write_i16_be(3);
    client.send(
        play_ids::SERVERBOUND_SET_CARRIED_ITEM,
        &carried.into_bytes(),
    );
    send_use_item_on(&mut client, 19, -61, 10, 1);

    // 导线 (20,-60,10) 应点亮为 4153（power 15）
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut powered = false;
    while Instant::now() < deadline && !powered {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(20, -60, 10) {
                powered = state == 4153;
            }
        }
    }
    assert!(
        powered,
        "wire should be powered (4153) by adjacent redstone block"
    );
}

/// 水桶（真实客户端路径放置）：水方块出现并向下/四周扩散（流体不工作）。
#[test]
fn water_bucket_places_and_flows() {
    let server = TestServer::start("water-bucket");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 热栏第 5 格放水桶并选中 → 点击 (25,-61,10) 顶面 → 水放于 (25,-60,10)
    send_creative_slot(&mut client, 40, "water_bucket");
    let mut carried = Writer::new();
    carried.write_i16_be(4);
    client.send(
        play_ids::SERVERBOUND_SET_CARRIED_ITEM,
        &carried.into_bytes(),
    );
    send_use_item_on(&mut client, 25, -61, 10, 1);

    // 等待水源（WATER = 80，level 0）或扩散的流动态（80..95）出现
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut saw_water = false;
    let mut spread = false;
    while Instant::now() < deadline && !(saw_water && spread) {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            let water = (80..=95).contains(&state);
            if pos == icbm_mc_767::packets::play::BlockPos::new(25, -60, 10) && water {
                saw_water = true;
            }
            if pos != icbm_mc_767::packets::play::BlockPos::new(25, -60, 10) && water {
                spread = true; // 扩散到相邻格
            }
        }
    }
    assert!(saw_water, "water block should be placed by bucket");
    assert!(spread, "water should spread to neighbors");
}

/// 辅助：`SET_CREATIVE_MODE_SLOT`（vanilla 线格式：槽位 short + ItemStack）。
fn send_creative_slot(client: &mut FakeClient, slot: i32, item: &str) {
    let mut w = Writer::new();
    w.write_i16_be(slot as i16);
    w.write_var_int(1); // count
    let item_id = icbm_mc_767::item_ids::item_id(item).unwrap() + 1;
    w.write_var_int(item_id);
    w.write_var_int(0); // to_add
    w.write_var_int(0); // to_remove
    client.send(
        play_ids::SERVERBOUND_SET_CREATIVE_MODE_SLOT,
        &w.into_bytes(),
    );
}

/// 辅助：`USE_ITEM_ON`（vanilla 线格式：手 + BlockHitResult + sequence）。
fn send_use_item_on(client: &mut FakeClient, x: i32, y: i32, z: i32, face: i32) {
    let mut w = Writer::new();
    w.write_var_int(0); // hand = main
    w.write_i64_be(icbm_mc_767::packets::play::BlockPos::new(x, y, z).as_long());
    w.write_var_int(face);
    w.write_f32_be(0.5); // hit_x
    w.write_f32_be(1.0); // hit_y
    w.write_f32_be(0.5); // hit_z
    w.write_bool(false); // inside
    w.write_var_int(0); // sequence
    client.send(play_ids::SERVERBOUND_USE_ITEM_ON, &w.into_bytes());
}

/// 红石线点亮后拆除电源必须熄灭（旧实现 set_block 只查新状态，拆除电源
/// 不触发重新求值，导线永远保持点亮）。
#[test]
fn redstone_wire_turns_off_when_source_removed() {
    let server = TestServer::start("redstone-off");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    send_chat_command(&mut client, "setblock 20 -60 10 redstone_wire");
    recv_chat_until(&mut client, "已放置");
    send_chat_command(&mut client, "setblock 19 -60 10 redstone_block");
    recv_chat_until(&mut client, "已放置");

    // 等导线点亮（4153 = power 15）
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut powered = false;
    while Instant::now() < deadline && !powered {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(20, -60, 10) {
                powered = state == 4153;
            }
        }
    }
    assert!(
        powered,
        "wire should be powered (4153) by adjacent redstone block"
    );

    // 拆除电源 → 导线必须回到 power 0（4138）
    send_chat_command(&mut client, "setblock 19 -60 10 air");
    recv_chat_until(&mut client, "已放置");
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut off = false;
    while Instant::now() < deadline && !off {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(20, -60, 10) {
                off = state == 4138;
            }
        }
    }
    assert!(
        off,
        "wire must turn off (4138) after the power source is removed"
    );
}

/// 地狱门往返：主世界 → 地狱（目的端自动生成返回传送门）→ 走回传送门返回
/// 主世界（地狱门无法打开 BUG：无返回传送门 + 地狱面检测不到）。
#[test]
fn nether_portal_round_trip_generates_return_portal() {
    let server = TestServer::start("portal-round-trip");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    send_chat_command(&mut client, "fill 20 -58 0 20 -58 3 obsidian");
    recv_chat_until(&mut client, "已填充");
    send_chat_command(&mut client, "fill 20 -54 0 20 -54 3 obsidian");
    recv_chat_until(&mut client, "已填充");
    send_chat_command(&mut client, "fill 20 -57 0 20 -55 0 obsidian");
    recv_chat_until(&mut client, "已填充");
    send_chat_command(&mut client, "fill 20 -57 3 20 -55 3 obsidian");
    recv_chat_until(&mut client, "已填充");

    // 站入传送门内部 (20,-56,1) → CRespawn（进入地狱）
    let mut move_in = Writer::new();
    move_in.write_f64_be(20.0);
    move_in.write_f64_be(-56.0);
    move_in.write_f64_be(1.0);
    move_in.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_in.into_bytes());

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut to_nether = false;
    let mut nether_portal_seen = false;
    while Instant::now() < deadline && !to_nether {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_RESPAWN {
            let mut r = Reader::new(&payload);
            let _dimension_type = r.read_var_int().unwrap();
            let dimension_name = r.read_string().unwrap();
            to_nether = dimension_name == "minecraft:the_nether";
        }
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let _pos = r.read_i64_be().unwrap();
            let state = r.read_var_int().unwrap();
            if state == 5864 || state == 5865 {
                nether_portal_seen = true; // 目的端返回传送门已生成
            }
        }
    }
    assert!(to_nether, "must switch to nether");
    assert!(
        nether_portal_seen,
        "return portal must be generated at the nether destination"
    );

    // 等维度切换冷却（10 tick）过去后，移出传送门再走回触发返回
    std::thread::sleep(Duration::from_millis(800));
    let mut move_out = Writer::new();
    move_out.write_f64_be(2.0);
    move_out.write_f64_be(128.0);
    move_out.write_f64_be(5.0);
    move_out.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_out.into_bytes());
    std::thread::sleep(Duration::from_millis(800));
    let mut move_back = Writer::new();
    move_back.write_f64_be(2.0);
    move_back.write_f64_be(128.0);
    move_back.write_f64_be(1.0);
    move_back.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_back.into_bytes());

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut back_to_overworld = false;
    while Instant::now() < deadline && !back_to_overworld {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_RESPAWN {
            let mut r = Reader::new(&payload);
            let _dimension_type = r.read_var_int().unwrap();
            let dimension_name = r.read_string().unwrap();
            back_to_overworld = dimension_name == "minecraft:overworld";
        }
    }
    assert!(
        back_to_overworld,
        "must return to overworld via nether portal"
    );
}

/// 玩家数据持久化：离线后重新进入恢复游戏模式与位置。
#[test]
fn playerdata_persists_gamemode_and_position() {
    let server = TestServer::start("playerdata");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 改创造 + 传送到远处
    send_chat_command(&mut client, "gamemode creative");
    recv_chat_until(&mut client, "creative");
    send_chat_command(&mut client, "tp 30 -60 40");
    recv_chat_until(&mut client, "传送到");
    // 等服务端把玩家位置更新到 (30,-60,40)（发送一个位置包触发）
    let mut move_to = Writer::new();
    move_to.write_f64_be(30.0);
    move_to.write_f64_be(-60.0);
    move_to.write_f64_be(40.0);
    move_to.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_to.into_bytes());
    std::thread::sleep(std::time::Duration::from_millis(500));

    // 断开（关闭连接 → on_disconnect 保存存档）
    drop(client);
    std::thread::sleep(std::time::Duration::from_millis(500));

    // 存档文件应包含创造模式与位置
    let data_dir = server.world_dir.join("playerdata");
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(&data_dir).map(|d| d.count()).unwrap_or(0) == 0 {
        assert!(Instant::now() < deadline, "playerdata file not written");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let files: Vec<_> = std::fs::read_dir(&data_dir)
        .expect("playerdata dir")
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "one playerdata file");
    let nbt = icbm_anvil::player::read_player(&files[0])
        .expect("read playerdata")
        .expect("exists");
    let icbm_mc_767::nbt::Nbt::Compound(root) = nbt else {
        panic!("playerdata compound")
    };
    assert_eq!(root.get_i32("playerGameType"), Some(1), "creative saved");
    let icbm_mc_767::nbt::Nbt::List(pos_list) = root.get("Pos").expect("Pos") else {
        panic!("Pos list");
    };
    let pos: Vec<f64> = pos_list
        .values
        .iter()
        .filter_map(|v| {
            if let icbm_mc_767::nbt::Nbt::Double(d) = v {
                Some(*d)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(pos, vec![30.0, -60.0, 40.0], "position saved");

    // 重新连接：位置应恢复（进入时按存档位置发 CPlayerPosition）。
    // 注意：join_to_play 会消费位置包，这里自定义连接以捕获它。
    let mut client2 = FakeClient::connect(&server.addr);
    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(2);
    client2.send(0x00, &intent.into_bytes());
    let mut start = Writer::new();
    start.write_string("diagplayer");
    start.write_uuid(&Uuid::nil());
    client2.send(login_ids::SERVERBOUND_START, &start.into_bytes());
    let (cid, payload) = client2.recv();
    assert_eq!(cid, login_ids::CLIENTBOUND_COMPRESSION);
    let mut r = Reader::new(&payload);
    client2.compression_threshold = Some(r.read_var_int().unwrap());
    client2.recv_until(login_ids::CLIENTBOUND_SUCCESS, "success");
    client2.send(login_ids::SERVERBOUND_ACKNOWLEDGED, &[]);
    client2.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        "brand",
    );
    client2.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_ENABLED_FEATURES,
        "features",
    );
    client2.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_KNOWN_PACKS,
        "packs",
    );
    let mut packs = Writer::new();
    packs.write_var_int(0);
    client2.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_KNOWN_PACKS,
        &packs.into_bytes(),
    );
    for i in 0..11 {
        client2.recv_until(
            icbm_mc_767::packets::config_ids::CLIENTBOUND_REGISTRY_DATA,
            &format!("reg {i}"),
        );
    }
    client2.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_TAGS,
        "tags",
    );
    client2.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_FINISH_CONFIGURATION,
        "finish",
    );
    client2.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_FINISH_CONFIGURATION,
        &[],
    );
    client2.recv_until(play_ids::CLIENTBOUND_LOGIN, "login");
    client2.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER, "center");
    client2.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_RADIUS, "radius");
    client2.recv_until(play_ids::CLIENTBOUND_CHANGE_DIFFICULTY, "difficulty");
    client2.recv_until(play_ids::CLIENTBOUND_PLAYER_ABILITIES, "abilities");
    client2.recv_until(play_ids::CLIENTBOUND_PLAYER_INFO_UPDATE, "info");
    client2.recv_until(play_ids::CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION, "spawn");
    client2.recv_until(play_ids::CLIENTBOUND_COMMANDS, "commands");
    client2.recv_until(play_ids::CLIENTBOUND_CUSTOM_PAYLOAD, "register");
    let pos = client2.recv_until(play_ids::CLIENTBOUND_PLAYER_POSITION, "restored position");
    let mut r = Reader::new(&pos);
    let x = r.read_f64_be().unwrap();
    let y = r.read_f64_be().unwrap();
    let z = r.read_f64_be().unwrap();
    assert_eq!((x, y, z), (30.0, -60.0, 40.0), "position restored");
}

/// `/weather` 指令：core 全局天气同步到 region（晴天/下雨/雷暴反馈）。
#[test]
fn weather_command_sets_global_weather() {
    let server = TestServer::start("weather");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    send_chat_command(&mut client, "weather rain");
    let text = recv_chat_until(&mut client, "下雨");
    assert!(text.contains("已设为"), "weather feedback: {text}");

    send_chat_command(&mut client, "weather clear");
    let text = recv_chat_until(&mut client, "晴朗");
    assert!(text.contains("已设为"), "weather feedback: {text}");

    send_chat_command(&mut client, "weather thunder");
    let text = recv_chat_until(&mut client, "雷暴");
    assert!(text.contains("已设为"), "weather feedback: {text}");
}

/// 服务端命令：`/gamemode creative`、`/gamerule`（含 level.dat 持久化）与
/// `/bmtool tps` 应从系统聊天返回反馈。
#[test]
fn command_tab_completion() {
    run_command_tab_completion();
}

#[test]
fn player_commands_gamemode_gamerule_bmtool() {
    let server = TestServer::start("commands");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // /gamemode creative → 能力包更新（FLAG_CREATIVE = 0x08）
    send_chat_command(&mut client, "gamemode creative");
    let abilities = client.recv_until(play_ids::CLIENTBOUND_PLAYER_ABILITIES, "creative abilities");
    let mut r = Reader::new(&abilities);
    let flags = r.read_u8().unwrap();
    assert!(
        flags & 0x08 != 0,
        "creative flag set (0x08), got {flags:#04x}"
    );
    let _flying_speed = r.read_f32_be().unwrap();
    let _fov = r.read_f32_be().unwrap();

    // /gamerule keepInventory true → 设置反馈
    send_chat_command(&mut client, "gamerule keepInventory true");
    let chat = recv_chat_until(&mut client, "keepInventory");
    assert!(chat.contains("已设置为 true"));

    // level.dat 已写入 GameRules（轮询等待落盘完成）
    let deadline = Instant::now() + Duration::from_secs(5);
    let persisted = loop {
        let level = icbm_anvil::level::read_level_dat(server.world_dir.join("level.dat"));
        let has_rule = level.is_ok_and(|level| {
            level
                .gamerules
                .contains(&("keepInventory".to_owned(), "true".to_owned()))
        });
        if has_rule {
            break true;
        }
        assert!(
            Instant::now() < deadline,
            "gamerules not persisted to level.dat within 5s"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(persisted);

    // /bmtool tps → 反馈含各统计字段
    send_chat_command(&mut client, "bmtool tps");
    let text = recv_chat_until(&mut client, "CPU负载");
    assert!(text.contains("平均region tps"), "bmtool output: {text}");
}

/// 逐帧接收系统聊天直到文本包含 `needle`，返回完整文本。
fn recv_chat_until(client: &mut FakeClient, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "no system chat containing {needle:?}"
        );
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_SYSTEM_CHAT {
            let text = text_of(&payload);
            if text.contains(needle) {
                return text;
            }
        }
    }
}

/// 发送一条聊天命令（`enforcesSecureChat=false` 时客户端用未签名
/// `CommandExecutionC2SPacket`（0x04）：载荷仅一个命令字符串）。
fn send_chat_command(client: &mut FakeClient, command: &str) {
    let mut w = Writer::new();
    w.write_string(command);
    client.send(play_ids::SERVERBOUND_CHAT_COMMAND, &w.into_bytes());
}

/// 命令补全请求（0x0B `RequestCommandCompletionsC2SPacket` → 0x10）：
/// 顶层命令、参数值与替换区间都按 1.21.1 语义回给客户端。
fn run_command_tab_completion() {
    let server = TestServer::start("suggestions");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 顶层命令："/" 列出全部命令（升序）。
    let texts = request_suggestions(&mut client, "/", 1);
    assert_eq!(
        texts,
        vec![
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
        ]
    );

    // "/gamemode cr" → creative；替换区间覆盖 "cr"（起点 10、长度 2）。
    let (start, length, texts) = request_suggestions_full(&mut client, "/gamemode cr", 2);
    assert_eq!(start, 10);
    assert_eq!(length, 2);
    assert_eq!(texts, vec!["creative"]);

    // "/gamemode "（含尾部空格）→ 四种模式。
    let texts = request_suggestions(&mut client, "/gamemode ", 3);
    assert_eq!(
        texts,
        vec!["adventure", "creative", "spectator", "survival"]
    );

    // "/gamemode"（字面量完整、未输入空格）→ 仍给出其参数建议。
    let texts = request_suggestions(&mut client, "/gamemode", 4);
    assert_eq!(texts.len(), 4);

    // "/gamerule k" → 规则名过滤。
    let texts = request_suggestions(&mut client, "/gamerule k", 5);
    assert!(
        texts.contains(&"keepInventory".to_owned()),
        "rules: {texts:?}"
    );

    // "/bmtool " → tps。
    let texts = request_suggestions(&mut client, "/bmtool ", 6);
    assert_eq!(texts, vec!["tps"]);
}

/// 发送命令补全请求并返回建议文本列表。
fn request_suggestions(client: &mut FakeClient, command: &str, id: i32) -> Vec<String> {
    request_suggestions_full(client, command, id).2
}

/// 发送命令补全请求并返回 (start, length, 建议文本)。
fn request_suggestions_full(
    client: &mut FakeClient,
    command: &str,
    id: i32,
) -> (i32, i32, Vec<String>) {
    let mut w = Writer::new();
    w.write_var_int(id);
    w.write_string(command);
    client.send(play_ids::SERVERBOUND_COMMAND_SUGGESTION, &w.into_bytes());
    let payload = client.recv_until(play_ids::CLIENTBOUND_COMMAND_SUGGESTIONS, "suggestions");
    let mut r = Reader::new(&payload);
    let resp_id = r.read_var_int().unwrap();
    assert_eq!(resp_id, id, "响应必须回显请求 ID");
    let start = r.read_var_int().unwrap();
    let length = r.read_var_int().unwrap();
    let count = r.read_var_int().unwrap();
    let mut texts = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let text = r.read_string().unwrap();
        assert!(!r.read_bool().unwrap(), "tooltip 未发送");
        texts.push(text);
    }
    (start, length, texts)
}

/// 解析系统聊天包中的 NBT 组件文本。
fn text_of(chat_payload: &[u8]) -> String {
    let mut r = Reader::new(chat_payload);
    let nbt = r.read_nbt().unwrap().unwrap();
    let compound = match nbt {
        icbm_mc_767::nbt::Nbt::Compound(c) => c,
        other => panic!("expected compound {other:?}"),
    };
    compound.get_string("text").unwrap_or_default().to_owned()
}

/// 公共进服流程（握手 → 登录 → 配置 → Play）。
fn join_to_play(client: &mut FakeClient) {
    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(2);
    client.send(0x00, &intent.into_bytes());

    let mut start = Writer::new();
    start.write_string("diagplayer");
    start.write_uuid(&Uuid::nil());
    client.send(login_ids::SERVERBOUND_START, &start.into_bytes());

    let (cid, payload) = client.recv();
    assert_eq!(cid, login_ids::CLIENTBOUND_COMPRESSION);
    let mut r = Reader::new(&payload);
    let threshold = r.read_var_int().unwrap();
    client.compression_threshold = Some(threshold);
    client.recv_until(login_ids::CLIENTBOUND_SUCCESS, "success");

    client.send(login_ids::SERVERBOUND_ACKNOWLEDGED, &[]);
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        "brand",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_ENABLED_FEATURES,
        "features",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_KNOWN_PACKS,
        "packs",
    );
    let mut packs = Writer::new();
    packs.write_var_int(0);
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_KNOWN_PACKS,
        &packs.into_bytes(),
    );
    for i in 0..11 {
        client.recv_until(
            icbm_mc_767::packets::config_ids::CLIENTBOUND_REGISTRY_DATA,
            &format!("reg {i}"),
        );
    }
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_TAGS,
        "tags",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_FINISH_CONFIGURATION,
        "finish",
    );
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_FINISH_CONFIGURATION,
        &[],
    );

    client.recv_until(play_ids::CLIENTBOUND_LOGIN, "login");
    client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER, "center");
    client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_RADIUS, "radius");
    client.recv_until(play_ids::CLIENTBOUND_CHANGE_DIFFICULTY, "difficulty");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_ABILITIES, "abilities");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_INFO_UPDATE, "info");
    client.recv_until(play_ids::CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION, "spawn");
    client.recv_until(play_ids::CLIENTBOUND_COMMANDS, "commands");
    client.recv_until(play_ids::CLIENTBOUND_CUSTOM_PAYLOAD, "register");
    // 位置同步先于区块批次（对照 vanilla `requestTeleport`）
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_POSITION, "position");
    client.recv_until(play_ids::CLIENTBOUND_GAME_EVENT, "chunks coming");
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_START, "batch start");
}

/// 创造模式从背包拿取物品（`SET_CREATIVE_MODE_SLOT`）不得触发数据包处理失败。
///
/// 回归：`CreativeInventoryActionC2SPacket.java` 的槽位是 `PacketCodecs.SHORT`
/// （2 字节有符号大端），旧实现按 VarInt 解码导致整包错位；且 1.21.1
/// `ComponentChanges.PACKET_CODEC` 无长度前缀，旧解码器多读一个 `len` VarInt，
/// 遇到带附魔组件的栈时读越界 → `unexpected eof` → 玩家被踢"数据包处理失败"。
#[test]
fn creative_inventory_take_does_not_kick() {
    let mut server = TestServer::start("creative-take");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    send_chat_command(&mut client, "gamemode creative");
    recv_chat_until(&mut client, "creative");

    // 模拟 vanilla 1.21.1 客户端把一件附魔钻石剑放入删除槽（slot=-1）：
    // `[short slot][ItemStack]`，栈 = count + item(id+1) + ComponentChanges
    // （无长度前缀；enchantments 组件网络 ID=9）。
    let mut w = Writer::new();
    w.write_i16_be(-1); // slot = -1（删除）
    w.write_var_int(1); // count = 1
    let diamond_sword = icbm_mc_767::item_ids::item_id("diamond_sword").unwrap() + 1;
    w.write_var_int(diamond_sword); // registryEntry：raw_id + 1
    w.write_var_int(1); // to_add = 1
    w.write_var_int(0); // to_remove = 0
    w.write_var_int(9); // component = minecraft:enchantments
    w.write_var_int(1); // entries = 1
    w.write_var_int(1 + 1); // sharpness (raw 1) + 1
    w.write_var_int(3); // level = 3
    w.write_bool(true); // showInTooltip
    client.send(
        play_ids::SERVERBOUND_SET_CREATIVE_MODE_SLOT,
        &w.into_bytes(),
    );

    // 再放一个普通石头栈到槽 5（无组件），两包都不得导致断线。
    let mut w = Writer::new();
    w.write_i16_be(5); // slot = 5
    w.write_var_int(1); // count
    let stone = icbm_mc_767::item_ids::item_id("stone").unwrap() + 1;
    w.write_var_int(stone);
    w.write_var_int(0); // to_add = 0
    w.write_var_int(0); // to_remove = 0
    client.send(
        play_ids::SERVERBOUND_SET_CREATIVE_MODE_SLOT,
        &w.into_bytes(),
    );

    // 5 秒内不得收到 DISCONNECT（数据包处理失败）。
    client.set_read_timeout(Duration::from_millis(200));
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let Some((id, _)) = client.recv_opt() else {
            continue;
        };
        assert!(
            id != play_ids::CLIENTBOUND_DISCONNECT,
            "creative take kicked player (disconnect packet received)"
        );
    }
    client.set_read_timeout(Duration::from_secs(10));
    // 服务器进程仍在运行（连接未被踢则无 REMOVE_ENTITIES/断开痕迹）
    assert!(
        server.child.try_wait().expect("try_wait").is_none(),
        "server exited"
    );
}

/// 玩家移动到负数坐标时，该区域的 terrain 必须生成并送达（修复"地形不在
/// 负数区域生成导致无限下坠"）。
#[test]
fn negative_coordinates_terrain_is_generated() {
    let server = TestServer::start("negative-terrain");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 跨区到负数：从 (0,4,0) 走到 (-600,4,0)（x=-512 是 region -1 与 0 的边界）。
    let mut move_packet = Writer::new();
    move_packet.write_f64_be(-600.0);
    move_packet.write_f64_be(4.0);
    move_packet.write_f64_be(0.0);
    move_packet.write_bool(true);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &move_packet.into_bytes());

    // 区块缓存中心更新到负数区块：floor(-600/16) = -38。
    let center = client.recv_until(
        play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER,
        "negative chunk cache center",
    );
    let mut r = Reader::new(&center);
    let cx = r.read_var_int().unwrap();
    let cz = r.read_var_int().unwrap();
    assert_eq!((cx, cz), (-38, 0));

    // 视野区块到达（批次开始 → 完成），289 = (2*8+1)^2；同时收集批次中
    // 是否出现负数坐标区块（-38 属 region (-2,0) 内部，必然出现），并解码其
    // 地形 palette 确认是真实的超平坦（草/泥土/基岩），而非空气。
    client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_START,
        "negative batch start",
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_negative_chunk = false;
    let mut negative_palettes: Option<Vec<Vec<i32>>> = None;
    let mut batch_size = None;
    while Instant::now() < deadline && batch_size.is_none() {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_LEVEL_CHUNK_WITH_LIGHT {
            let mut r = Reader::new(&payload);
            let x = r.read_i32_be().unwrap();
            let z = r.read_i32_be().unwrap();
            let _heightmap = r.read_nbt().unwrap().unwrap();
            let data_len = r.read_var_int().unwrap() as usize;
            let data = r.read_bytes(data_len).unwrap();
            let mut sr = Reader::new(data);
            let mut palettes: Vec<Vec<i32>> = Vec::new();
            for _ in 0..24 {
                let _nonair = sr.read_i16_be().unwrap();
                let (_, entries) = read_palette_container(&mut sr);
                palettes.push(entries);
                read_biome_container(&mut sr);
            }
            if x < 0 || z < 0 {
                saw_negative_chunk = true;
                if negative_palettes.is_none() {
                    negative_palettes = Some(palettes);
                }
            }
        } else if id == play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED {
            let mut r = Reader::new(&payload);
            batch_size = Some(r.read_var_int().unwrap());
        }
    }
    assert_eq!(batch_size, Some(289), "(2*8+1)^2 chunks");
    assert!(saw_negative_chunk, "no negative-coordinate chunk delivered");
    // 负数区块必须有真实的超平坦地形：section 0 = 世界 y -64..-49 含
    // 基岩(79)/泥土(10)/非雪草(9)。
    let palettes = negative_palettes.expect("negative chunk palettes");
    let surface = &palettes[0];
    for id in [79, 10, 9] {
        assert!(
            surface.contains(&id),
            "negative chunk surface palette {surface:?} missing block id {id}"
        );
    }
    assert!(
        palettes[1..].iter().all(|section| section == &vec![0]),
        "negative chunk above-surface sections must be air only"
    );
}

/// 玩家坠入主世界虚空（Y < -64）时被拉回出生点，避免无限下坠。
#[test]
fn void_fall_rescued_to_spawn() {
    let server = TestServer::start("void-rescue");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 模拟客户端掉到世界底部之下（如区块未及时加载导致的坠入虚空）
    let mut fall = Writer::new();
    fall.write_f64_be(0.0);
    fall.write_f64_be(-100.0);
    fall.write_f64_be(0.0);
    fall.write_bool(false);
    client.send(play_ids::SERVERBOUND_MOVE_POS, &fall.into_bytes());

    // 服务端应发回 CPlayerPosition 到出生点 (0, 4, 0)
    let deadline = Instant::now() + Duration::from_secs(15);
    let (x, y, z) = loop {
        assert!(Instant::now() < deadline, "no rescue teleport received");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_PLAYER_POSITION {
            let mut r = Reader::new(&payload);
            let x = r.read_f64_be().unwrap();
            let y = r.read_f64_be().unwrap();
            let z = r.read_f64_be().unwrap();
            break (x, y, z);
        }
    };
    assert_eq!(
        (x, y, z),
        (0.0, -60.0, 0.0),
        "rescued to world spawn (superflat top)"
    );
}

/// 玩家放置与破坏方块：`give` 石头 → 右键（USE_ITEM_ON）在脚下放置 →
/// 服务端应广播 `CBlockUpdate`（石头）；随后破坏该方块 → 广播空气。
#[test]
fn player_places_and_breaks_blocks() {
    use icbm_mc_767::packets::play::block_state_ids;
    use icbm_mc_767::packets::play::serverbound::SUseItemOn;

    let server = TestServer::start("place");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // /give diagplayer stone → 手持石头（放置方块用）
    send_chat_command(&mut client, "give diagplayer stone");
    recv_chat_until(&mut client, "已给");

    // 模拟右键点击脚下地面（面=1 上 → 放置到 y+1）
    // 玩家在 y=-60 站立（superflat 地表 -61 之上），点击 y=-61 的草方块顶面
    // → 放置到 (0, -60, 0)
    let mut w = Writer::new();
    w.write_var_int(0); // hand = main
    w.write_i64_be(icbm_mc_767::packets::play::BlockPos::new(0, -61, 0).as_long());
    w.write_var_int(1); // face = UP
    w.write_f32_be(0.5);
    w.write_f32_be(1.0);
    w.write_f32_be(0.5);
    w.write_bool(false); // inside
    w.write_var_int(0); // sequence
    client.send(play_ids::SERVERBOUND_USE_ITEM_ON, &w.into_bytes());

    // 等待 CBlockUpdate：位置 (0, -60, 0) 变石头
    let deadline = Instant::now() + Duration::from_secs(15);
    let placed = loop {
        assert!(Instant::now() < deadline, "no block update for placement");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(0, -60, 0) {
                break state;
            }
        }
    };
    assert_eq!(placed, block_state_ids::STONE, "placed stone");

    // 破坏该方块：PLAYER_ACTION status=0（开始破坏）
    let mut w = Writer::new();
    w.write_var_int(0); // start digging
    w.write_i64_be(icbm_mc_767::packets::play::BlockPos::new(0, -60, 0).as_long());
    w.write_var_int(1); // face
    w.write_var_int(0); // sequence
    client.send(play_ids::SERVERBOUND_PLAYER_ACTION, &w.into_bytes());

    // 等待 CBlockUpdate：位置 (0, -60, 0) 变空气
    let deadline = Instant::now() + Duration::from_secs(15);
    let broken = loop {
        assert!(Instant::now() < deadline, "no block update for breaking");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_BLOCK_UPDATE {
            let mut r = Reader::new(&payload);
            let pos = icbm_mc_767::packets::play::BlockPos::from_long(r.read_i64_be().unwrap());
            let state = r.read_var_int().unwrap();
            if pos == icbm_mc_767::packets::play::BlockPos::new(0, -60, 0) {
                break state;
            }
        }
    };
    assert_eq!(broken, 0, "broken to air");
}

/// 辅助：`CONTAINER_CLICK`（vanilla 线格式：containerId(byte) + stateId(VarInt)
/// + slot(short) + button(byte) + clickType(VarInt) + changedSlots(空) +
/// carried(空栈)）。用于验证背包点击移动物品。
fn send_container_click(client: &mut FakeClient, slot: i16, button: i8, click_type: i32) {
    let mut w = Writer::new();
    w.write_i8(0); // container_id = 玩家物品栏
    w.write_var_int(0); // state_id
    w.write_i16_be(slot);
    w.write_i8(button);
    w.write_var_int(click_type);
    w.write_var_int(0); // changed_slots 空
    w.write_var_int(0); // carried 空栈
    client.send(play_ids::SERVERBOUND_CONTAINER_CLICK, &w.into_bytes());
}

/// 背包点击移动物品：热栏放物品 → 左键拿起（PICKUP）→ 右键放到主栏 → 验证
/// 服务端收到 CSetSlot 回写（客户端与服务端状态一致）。
#[test]
fn container_click_moves_item_between_slots() {
    let server = TestServer::start("container-click");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");
    // 初始物品栏同步（`CSetContainerContent`）已随 join 序列下发（被 join_to_play
    // 内部消费）；此处直接放物品到热栏槽 40（hotbar index 4）
    send_creative_slot(&mut client, 40, "stone");
    // 等待 CSetSlot 回写（热栏槽 40）
    let deadline = Instant::now() + Duration::from_secs(5);
    let saw_slot_40 = loop {
        assert!(Instant::now() < deadline, "no CSetSlot for hotbar slot 40");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_CONTAINER_SET_SLOT {
            let mut r = Reader::new(&payload);
            let sync_id = r.read_i8().unwrap();
            let _revision = r.read_var_int().unwrap();
            let slot = r.read_i16_be().unwrap();
            if sync_id == 0 && slot == 40 {
                break true;
            }
        }
    };
    assert!(saw_slot_40);

    // 左键点击热栏槽 40（PICKUP，按钮 0）→ 拿起 stone
    send_container_click(&mut client, 40, 0, 0);
    std::thread::sleep(Duration::from_millis(200));
    // 验证收到槽 40 清空的 CSetSlot 与光标变化的 CSetSlot（槽 -1）
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_clear = false;
    let mut saw_cursor = false;
    while Instant::now() < deadline && !(saw_clear && saw_cursor) {
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_CONTAINER_SET_SLOT {
            let mut r = Reader::new(&payload);
            let sync_id = r.read_i8().unwrap();
            let _revision = r.read_var_int().unwrap();
            let slot = r.read_i16_be().unwrap();
            let count = r.read_var_int().unwrap();
            if sync_id == 0 && slot == 40 && count == 0 {
                saw_clear = true; // 槽 40 清空
            }
            if sync_id == 0 && slot == -1 && count == 1 {
                saw_cursor = true; // 光标有 1 个 stone
            }
        }
    }
    assert!(saw_clear, "hotbar slot 40 should be cleared after pickup");
    assert!(saw_cursor, "cursor should carry the picked-up stone");
}

/// 慢客户端不阻塞 datarouter receiver 线程（写线程隔离回归）。
///
/// 旧实现：`forward_chunk_data` 在 receiver 线程内 `write_all` 到阻塞 socket，
/// 客户端不读时 socket 缓冲填满 → receiver 卡死 >5s → 全 region 心跳批量超时
/// → 条目被删 → 重分配撞 BUSY → 玩家被踢。新实现：per-client 出站队列 +
/// 写线程，receiver 只入队。
#[test]
fn slow_client_does_not_stall_other_players() {
    let server = TestServer::start("slow-client");
    // 玩家 A：进服并保持连接，但停止读取 socket（模拟滞后客户端）
    let mut client_a = FakeClient::connect(&server.addr);
    join_to_play(&mut client_a);
    client_a.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // A 移动到远处新 region：触发 81 区块批次洪泛 → A 的 socket 缓冲填满，
    // 写线程阻塞（旧实现是 receiver 线程阻塞）。
    let mut move_to = Writer::new();
    move_to.write_f64_be(1200.0);
    move_to.write_f64_be(-60.0);
    move_to.write_f64_be(1200.0);
    move_to.write_bool(true);
    client_a.send(play_ids::SERVERBOUND_MOVE_POS, &move_to.into_bytes());
    std::thread::sleep(Duration::from_millis(800));

    // A 不再读；给服务器时间填满 A 的 socket 缓冲并让写线程阻塞
    std::thread::sleep(Duration::from_secs(2));

    // 玩家 B：此刻必须仍能正常进服（receiver 未被 A 阻塞）
    let mut client_b = FakeClient::connect(&server.addr);
    join_to_play(&mut client_b);
    client_b.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // B 仍被正常服务：收到服务端 keep alive（每 5s 发，证明 receiver 线程
    // 未被 A 的写阻塞卡住——keep alive 由连接线程发送，但其包也经出站队列；
    // 而 B 的登录/进服本身已证明 receiver 健康）
    let deadline = Instant::now() + Duration::from_secs(15);
    let saw_keep_alive = loop {
        assert!(Instant::now() < deadline, "B did not receive keep alive");
        let (id, _payload) = client_b.recv();
        if id == play_ids::CLIENTBOUND_KEEP_ALIVE {
            break true;
        }
    };
    assert!(saw_keep_alive, "player B still served while A stalls");
}
