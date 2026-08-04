//! 端到端连接集成测试：模拟真实 1.21.1 客户端走完
//! 握手 → 状态 → 登录 → 配置 → Play（进服）全流程。
//!
//! 覆盖：嵌入式 switch 拉起、core 网络状态机、region 子进程注册、
//! 区块生成与 IPC 转发、以及协议 767 包 ID 与字段布局。
//!
//! 需要 `target/debug/icbm`（Cargo 提供 `CARGO_BIN_EXE_icbm`）。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aes::Aes128;
use icbm_mc_767::codec::{Reader, Writer};
use icbm_mc_767::packets::login_ids;
use icbm_mc_767::packets::play::clientbound::CGameEvent;
use icbm_mc_767::packets::play_ids;
use icbm_mc_767::packets::status_ids;
use icbm_mc_767::packets::{decode_frame, encode_frame};
use icbm_mc_767::protocol::PROTOCOL_VERSION;
use uuid::Uuid;

/// 模拟客户端：帧编解码 + 压缩状态 + 收包超时。
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
        let frame =
            encode_frame(packet_id, payload, self.compression_threshold).expect("encode frame");
        self.stream.write_all(&frame).expect("write frame");
    }

    /// 读一个完整帧；返回 (包 ID, 载荷)。
    fn recv(&mut self) -> (i32, Vec<u8>) {
        loop {
            match decode_frame(&self.buffer, self.compression_threshold) {
                Ok(Some((id, payload, consumed))) => {
                    self.buffer.drain(..consumed);
                    return (id, payload);
                }
                Ok(None) => {
                    let mut chunk = [0u8; 8192];
                    let n = self.stream.read(&mut chunk).expect("read frame data");
                    assert!(n > 0, "server closed connection mid-frame");
                    self.buffer.extend_from_slice(&chunk[..n]);
                }
                Err(err) => panic!("server sent malformed frame: {err}"),
            }
        }
    }

    /// 读帧直到指定包 ID 出现（丢弃途中其它包），返回其载荷。
    fn recv_until(&mut self, packet_id: i32, what: &str) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            assert!(Instant::now() < deadline, "timeout waiting for {what}");
            let (id, payload) = self.recv();
            if id == packet_id {
                return payload;
            }
        }
    }
}

/// 测试用服务器句柄。
struct TestServer {
    child: Child,
    addr: String,
    world_dir: std::path::PathBuf,
    shm_name: String,
}

impl TestServer {
    /// `name` 用于区分并行测试实例（世界目录与 datarouter shm 名需唯一）。
    fn start(name: &str) -> Self {
        Self::start_with_mode(name, false)
    }

    /// 以指定在线模式启动（`online=true` 为正版验证）。
    fn start_with_mode(name: &str, online: bool) -> Self {
        let world_dir =
            std::env::temp_dir().join(format!("icbm-join-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&world_dir);
        std::fs::create_dir_all(&world_dir).unwrap();

        // 随机空闲端口
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let addr = format!("127.0.0.1:{port}");
        let shm_name = format!("/datarouter-join-test-{}-{name}", std::process::id());

        let exe = env!("CARGO_BIN_EXE_icbm");
        let child = Command::new(exe)
            .args([
                "--world",
                world_dir.to_str().unwrap(),
                "--port",
                &port.to_string(),
                "--shm",
                &shm_name,
            ])
            .env(
                "ICBM_ONLINE_MODE",
                if online { "online" } else { "offline" },
            )
            .process_group(0) // 独立进程组：Drop 时整组清理 region 子进程
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn icbm server");

        // 等待端口就绪
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if std::net::TcpStream::connect(&addr).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "server did not start listening");
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

/// 状态流程：握手（`next_state=1`）→ 状态请求 → MOTD JSON 响应 → ping → pong 回显。
///
/// 覆盖 join 流程未走到的 Status 状态机分支（MOTD 与 favicon 内嵌资源）。
#[test]
fn status_request_ping_flow() {
    let server = TestServer::start("status");
    let mut client = FakeClient::connect(&server.addr);

    // 握手 → 状态
    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(1);
    client.send(0x00, &intent.into_bytes());

    // 状态请求（空载荷）→ 响应 JSON（载荷为长度前缀字符串）
    client.send(status_ids::SERVERBOUND_REQUEST, &[]);
    let (id, payload) = client.recv();
    assert_eq!(id, status_ids::CLIENTBOUND_RESPONSE);
    let mut r = Reader::new(&payload);
    let json_str = r.read_string().unwrap();
    assert!(r.is_empty());
    let json: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert_eq!(json["version"]["protocol"], 767);
    assert_eq!(json["version"]["name"], "1.21.1");
    let favicon = json["favicon"].as_str().expect("favicon present");
    assert!(favicon.starts_with("data:image/png;base64,"));

    // ping → pong 原样回显（随后服务器关闭连接）
    let ping_payload = 0xDEAD_BEEF_CAFE_F00Du64 as i64;
    let mut w = Writer::new();
    w.write_i64_be(ping_payload);
    client.send(status_ids::SERVERBOUND_PING, &w.into_bytes());
    let (id, payload) = client.recv();
    assert_eq!(id, status_ids::CLIENTBOUND_PONG);
    let mut r = Reader::new(&payload);
    assert_eq!(r.read_i64_be().unwrap(), ping_payload);
}

// 全流程步骤较长；按读包顺序推进，拆分会损失顺序语义，故允许超长函数。
#[allow(clippy::too_many_lines)]
#[test]
fn full_join_flow_status_login_config_play() {
    let server = TestServer::start("join");
    let mut client = FakeClient::connect(&server.addr);

    // ── 握手（next state = 2 登录） ─────────────────────────────────────
    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(2);
    client.send(0x00, &intent.into_bytes());

    // ── 登录开始 ───────────────────────────────────────────────────────
    let mut start = Writer::new();
    start.write_string("testplayer");
    start.write_uuid(&Uuid::nil());
    client.send(login_ids::SERVERBOUND_START, &start.into_bytes());

    // 服务器应先发设置压缩（0x03，未压缩），随后登录成功（0x02，已压缩）
    let (compression_id, compression_payload) = client.recv();
    assert_eq!(compression_id, login_ids::CLIENTBOUND_COMPRESSION);
    let mut r = Reader::new(&compression_payload);
    let threshold = r.read_var_int().unwrap();
    assert!(threshold > 0);
    client.compression_threshold = Some(threshold);

    let success = client.recv_until(login_ids::CLIENTBOUND_SUCCESS, "login success");
    let mut r = Reader::new(&success);
    let uuid = r.read_uuid().unwrap();
    let name = r.read_string().unwrap();
    assert_eq!(name, "testplayer");
    assert_ne!(uuid, Uuid::nil());
    assert_eq!(r.read_var_int().unwrap(), 0); // 属性数
    assert!(r.read_bool().unwrap()); // strict error handling

    // ── 登录确认 → 配置阶段 ─────────────────────────────────────────────
    client.send(login_ids::SERVERBOUND_ACKNOWLEDGED, &[]);

    // 服务器先发 brand（custom payload 0x01）
    let brand = client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_CUSTOM_PAYLOAD,
        "brand custom payload",
    );
    let mut r = Reader::new(&brand);
    assert_eq!(r.read_string().unwrap(), "minecraft:brand");

    // 特性与 known packs
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_ENABLED_FEATURES,
        "enabled features",
    );
    let known_packs = client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_KNOWN_PACKS,
        "known packs",
    );
    let mut r = Reader::new(&known_packs);
    let count = r.read_var_int().unwrap();
    assert!(count >= 0);

    // 客户端回空 known packs → 服务器下发注册表
    let mut packs = Writer::new();
    packs.write_var_int(0);
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_KNOWN_PACKS,
        &packs.into_bytes(),
    );

    // 官方同步 11 个注册表 + 标签 + 完成配置
    for i in 0..11 {
        client.recv_until(
            icbm_mc_767::packets::config_ids::CLIENTBOUND_REGISTRY_DATA,
            &format!("registry data #{i}"),
        );
    }
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_UPDATE_TAGS,
        "update tags",
    );
    client.recv_until(
        icbm_mc_767::packets::config_ids::CLIENTBOUND_FINISH_CONFIGURATION,
        "finish configuration",
    );

    // ── 完成配置 → Play ─────────────────────────────────────────────────
    client.send(
        icbm_mc_767::packets::config_ids::SERVERBOUND_FINISH_CONFIGURATION,
        &[],
    );

    // 加入游戏（0x2B）
    let login_payload = client.recv_until(play_ids::CLIENTBOUND_LOGIN, "join game");
    let mut r = Reader::new(&login_payload);
    assert_eq!(r.read_i32_be().unwrap(), 1); // entity id
    assert!(!r.read_bool().unwrap()); // hardcore
    let dim_count = r.read_var_int().unwrap();
    assert_eq!(dim_count, 3);
    assert_eq!(r.read_string().unwrap(), "minecraft:overworld");
    assert_eq!(r.read_string().unwrap(), "minecraft:the_nether");
    assert_eq!(r.read_string().unwrap(), "minecraft:the_end");
    assert_eq!(
        r.read_var_int().unwrap(),
        icbm_core::config::ServerConfig::default().max_players
    ); // max players
    assert!(r.read_var_int().unwrap() > 0); // view distance
    assert!(r.read_var_int().unwrap() > 0); // simulation distance
    assert!(!r.read_bool().unwrap()); // reduced debug
    assert!(r.read_bool().unwrap()); // respawn screen
    assert!(!r.read_bool().unwrap()); // limited crafting
    // spawn data：维度类型 + 1（registryEntry codec）
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_string().unwrap(), "minecraft:overworld");
    assert_eq!(r.read_i64_be().unwrap(), 0); // hashed seed
    assert_eq!(r.read_u8().unwrap(), 0); // gamemode
    assert_eq!(r.read_i8().unwrap(), -1); // previous gamemode
    assert!(!r.read_bool().unwrap()); // debug
    assert!(r.read_bool().unwrap()); // flat
    assert!(!r.read_bool().unwrap()); // death location
    assert_eq!(r.read_var_int().unwrap(), 0); // portal cooldown
    assert!(!r.read_bool().unwrap()); // enforces secure chat

    // 区块缓存中心/半径
    let center = client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_CENTER, "center");
    let mut r = Reader::new(&center);
    let _ = r.read_var_int().unwrap();
    let _ = r.read_var_int().unwrap();
    let radius = client.recv_until(play_ids::CLIENTBOUND_SET_CHUNK_CACHE_RADIUS, "radius");
    let mut r = Reader::new(&radius);
    let view = r.read_var_int().unwrap();
    assert!(view >= 2);

    // 难度 / 能力 / 玩家信息 / 出生点 / 命令树
    client.recv_until(play_ids::CLIENTBOUND_CHANGE_DIFFICULTY, "difficulty");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_ABILITIES, "abilities");
    client.recv_until(play_ids::CLIENTBOUND_PLAYER_INFO_UPDATE, "player info");
    client.recv_until(
        play_ids::CLIENTBOUND_SET_DEFAULT_SPAWN_POSITION,
        "spawn position",
    );
    client.recv_until(play_ids::CLIENTBOUND_COMMANDS, "commands");

    // ICBM 通道注册（minecraft:register）
    let reg = client.recv_until(play_ids::CLIENTBOUND_CUSTOM_PAYLOAD, "minecraft:register");
    let mut r = Reader::new(&reg);
    assert_eq!(r.read_string().unwrap(), "minecraft:register");
    // payload 为 NUL 分隔的原始通道名（对照 DinnerboneProtocolUtils.writeChannels）
    let channels = r.read_rest();
    assert!(
        channels
            .split(|&b| b == 0)
            .any(|part| part == b"icbm_client:state_subscription"),
        "register channels: {channels:?}"
    );

    // 位置同步：先于区块批次发送（对照 vanilla `PlayerManager#onPlayerConnect` 的
    // `requestTeleport`）。客户端本地玩家初始在默认共享出生点 (8,64,8)，若延迟到
    // 批次后才同步会从高空坠入虚空。
    let pos = client.recv_until(play_ids::CLIENTBOUND_PLAYER_POSITION, "player position");
    let mut r = Reader::new(&pos);
    let _x = r.read_f64_be().unwrap();
    let _y = r.read_f64_be().unwrap();
    let _z = r.read_f64_be().unwrap();
    let _yaw = r.read_f32_be().unwrap();
    let _pitch = r.read_f32_be().unwrap();
    assert_eq!(r.read_u8().unwrap(), 0); // flags
    let _teleport_id = r.read_var_int().unwrap();

    // 初始区块即将到来
    let ev = client.recv_until(play_ids::CLIENTBOUND_GAME_EVENT, "initial chunks coming");
    let mut r = Reader::new(&ev);
    assert_eq!(
        r.read_u8().unwrap(),
        CGameEvent::EVENT_INITIAL_CHUNKS_COMING
    );
    let _ = r.read_f32_be().unwrap();

    // 区块批次开始 → 各区块（0x27）→ 批次完成
    client.recv_until(play_ids::CLIENTBOUND_CHUNK_BATCH_START, "chunk batch start");
    let total = (2 * view + 1) as i64 * (2 * view + 1) as i64;
    let batch_finished = client.recv_until(
        play_ids::CLIENTBOUND_CHUNK_BATCH_FINISHED,
        "chunk batch finished",
    );
    // 逐帧统计途中经过的区块（recv_until 已丢弃它们，这里无法直接数；
    // 用服务器诊断：chunk_diag 计数在 core 日志可见，此处只验证批次完成）
    assert!(!batch_finished.is_empty());
    let mut r = Reader::new(&batch_finished);
    assert_eq!(r.read_var_int().unwrap(), total as i32);
    // 玩家已进入世界：respond to a keep-alive to prove the session is live
    let ka = client.recv_until(play_ids::CLIENTBOUND_KEEP_ALIVE, "keep alive");
    let mut r = Reader::new(&ka);
    let ka_id = r.read_i64_be().unwrap();
    let mut resp = Writer::new();
    resp.write_i64_be(ka_id);
    client.send(play_ids::SERVERBOUND_KEEP_ALIVE, &resp.into_bytes());
}

/// 正版登录（RSA 加密 + AES-CFB8）失败时：客户端收到的踢包必须能正确解密
/// 且是 Login 状态的有效包（0x00 DISCONNECT）。回归：曾出现客户端报
/// "Internal Exception: Received unknown packet id 84"——加密流错位/包 ID 错误。
///
/// 服务器以默认正版模式启动（本测试环境无 Mojang 会话 → 认证必然失败），
/// 验证失败后的加密 DISCONNECT 可被客户端正常解密解析。
#[test]
fn online_auth_failure_sends_valid_encrypted_disconnect() {
    use aes::cipher::KeyInit;
    use rsa::Pkcs1v15Encrypt;
    use rsa::RsaPublicKey;
    use rsa::pkcs8::DecodePublicKey;

    // 本测试验证正版登录踢包（与其它测试的离线模式隔离）
    let server = TestServer::start_with_mode("online-kick", true);
    let mut client = FakeClient::connect(&server.addr);

    // 握手 → 登录
    let mut intent = Writer::new();
    intent.write_var_int(PROTOCOL_VERSION);
    intent.write_string("127.0.0.1");
    intent.write_u16_be(25565);
    intent.write_var_int(2);
    client.send(0x00, &intent.into_bytes());
    let mut start = Writer::new();
    start.write_string("authfailplayer");
    start.write_uuid(&Uuid::nil());
    client.send(login_ids::SERVERBOUND_START, &start.into_bytes());

    // 服务器应发 CEncryptionRequest（0x01）
    let (cid, payload) = client.recv();
    assert_eq!(cid, login_ids::CLIENTBOUND_HELLO, "encryption request");
    let mut r = Reader::new(&payload);
    let _server_id = r.read_string().unwrap();
    let public_key = r.read_byte_array().unwrap().to_vec();
    let verify_token = r.read_byte_array().unwrap().to_vec();
    let _needs_auth = r.read_bool().unwrap();

    // 客户端：生成 16 字节共享密钥，用服务器公钥加密密钥与 token
    let secret: [u8; 16] = [0x42; 16];
    let pubkey = RsaPublicKey::from_public_key_der(&public_key).expect("parse pubkey");
    let enc_secret = pubkey
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &secret)
        .expect("encrypt secret");
    let enc_token = pubkey
        .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, &verify_token)
        .expect("encrypt token");
    let mut key_packet = Writer::new();
    key_packet.write_byte_array(&enc_secret);
    key_packet.write_byte_array(&enc_token);
    client.send(login_ids::SERVERBOUND_KEY, &key_packet.into_bytes());

    // 客户端启用解密（AES-CFB8，key=secret；与服务器相同）
    let mut cipher = crate_net_crypto_like_cfb8(&secret);

    // 读取响应：登录阶段（无压缩），解密后应为 0x00 DISCONNECT
    let deadline = Instant::now() + Duration::from_secs(10);
    let (id, payload) = loop {
        assert!(Instant::now() < deadline, "no encrypted response");
        let mut chunk = [0u8; 8192];
        let n = client.stream.read(&mut chunk).expect("read");
        assert!(n > 0);
        cipher.decrypt(&mut chunk[..n]);
        client.buffer.extend_from_slice(&chunk[..n]);
        match decode_frame(&client.buffer, None) {
            Ok(Some((id, payload, consumed))) => {
                client.buffer.drain(..consumed);
                break (id, payload);
            }
            Ok(None) => continue,
            Err(err) => panic!("malformed encrypted frame: {err}"),
        }
    };
    assert_eq!(
        id,
        login_ids::CLIENTBOUND_DISCONNECT,
        "encrypted login disconnect (auth failure)"
    );
    // 1.21.1 登录断开理由为 JSON 字符串（`Text.Serialization.toJsonString`），
    // 不是 NBT——回归：曾写 NBT 导致客户端报 "unknown packet id"。
    let mut r = Reader::new(&payload);
    let json = r.read_string().expect("reason json string");
    assert!(
        json.contains("translate") || json.contains("text"),
        "reason JSON: {json}"
    );
    assert!(!json.is_empty());
    eprintln!("[online-kick] encrypted disconnect reason json: {json}");
}

/// 与 `core::net::crypto::AesCfb8` 相同的 AES-128-CFB8（key = IV = secret）。
fn crate_net_crypto_like_cfb8(key: &[u8; 16]) -> TestCfb8 {
    use aes::cipher::{KeyInit, generic_array::GenericArray};
    TestCfb8 {
        cipher: <Aes128 as KeyInit>::new(GenericArray::from_slice(key)),
        iv: *key,
    }
}

/// 测试用 AES-CFB8（与服务器实现逐字节一致）。
struct TestCfb8 {
    cipher: aes::Aes128,
    iv: [u8; 16],
}

impl TestCfb8 {
    fn decrypt(&mut self, buf: &mut [u8]) {
        use aes::cipher::BlockEncrypt;
        for byte in buf {
            let mut block = aes::Block::default();
            block.copy_from_slice(&self.iv);
            self.cipher.encrypt_block(&mut block);
            let out = *byte ^ block[0];
            self.iv.rotate_left(1);
            self.iv[15] = *byte;
            *byte = out;
        }
    }
}
