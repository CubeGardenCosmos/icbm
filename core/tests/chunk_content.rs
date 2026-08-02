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

struct TestServer {
    child: Child,
    addr: String,
    world_dir: std::path::PathBuf,
    shm_name: String,
}

impl TestServer {
    fn start(name: &str) -> Self {
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
            .process_group(0) // 独立进程组：Drop 时整组清理 region 子进程
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
    // section 索引 4 = 世界 y 0..15：底部 bedrock(y=0)，上方 dirt×2，顶层非 snowy 草(y=3)
    let surface = &palettes[4];
    for id in [79, 10, 9, 0] {
        assert!(
            surface.contains(&id),
            "surface palette {surface:?} missing block id {id} (bedrock=79 dirt=10 grass=9 air=0)"
        );
    }
    // 世界底部的 4 个 section（y -64..0）全部为空气，不含水
    for section in &palettes[0..4] {
        assert_eq!(
            section,
            &vec![0],
            "bottom section {section:?} must be air only (no water)"
        );
    }
    // 草方块必须是非 snowy 的全局 ID 9（8 是 snowy=true）
    let mut grass_found = false;
    for section in &palettes {
        if section.contains(&9) {
            grass_found = true;
        }
        assert!(
            !section.contains(&8),
            "snowy grass_block (id 8) must not be generated: {section:?}"
        );
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

/// region 模拟层随机生成漫游 Creeper（region 存活信号）：客户端应收到
/// `ADD_ENTITY`（实体类型 23 = creeper）。
#[test]
fn creeper_spawns_in_region() {
    let server = TestServer::start("creeper");
    let mut client = FakeClient::connect(&server.addr);
    join_to_play(&mut client);
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED, "initial batch");

    // 每 tick 1/120 概率，30 秒内生成概率 >99%；超时视为失败
    let deadline = Instant::now() + Duration::from_secs(30);
    let add = loop {
        assert!(Instant::now() < deadline, "no creeper spawned within 30s");
        let (id, payload) = client.recv();
        if id == play_ids::CLIENTBOUND_ADD_ENTITY {
            break payload;
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
    assert_eq!(y, 4.0, "creeper spawns on flat surface");
    assert!(entity_id >= 1000, "region entity ids start at 1000");
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
        vec!["about", "bmtool", "gamemode", "gamerule", "help"]
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
