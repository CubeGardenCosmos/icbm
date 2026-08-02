//! 控制台与日志（参考 master 分支 `core/src/logging.rs` 的控制台设计）。
//!
//! - **交互控制台**：stdin 为 TTY 时使用 rustyline（历史、高亮、Tab 补全、
//!   悬停提示），Ctrl-C / Ctrl-D 停止服务器；非 TTY 退化为逐行 stdin。
//! - **文件日志**：`logs/latest.log`，按天 gzip 滚动为 `logs/YYYY-MM-DD-N.log.gz`。
//! - **级别过滤**：`--log-level`（默认 INFO，`RUST_LOG` 环境变量可覆盖），
//!   TRACE/DEBUG/INFO/WARN/ERROR 严格分层（`LevelFilter`）。
//! - **region 子进程**：`use_stderr=true` 时日志写 stderr 且不启动交互控制台
//!   （core 模式的 stdout 留给控制台；region 经 datarouter 通讯，无 IPC 占用）。

#![allow(clippy::print_stderr)]
#![allow(clippy::print_stdout)]

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use flate2::write::GzEncoder;
use icbm_gameplay_icbm::bmtool::{BmToolCommand, format_bmtool_tps};
use icbm_gameplay_minecraft::commands::{self, Command, GameMode};
use crate::net::play::about_message;
use icbm_mc_767::packets::play::clientbound::{
    CGameEvent, CPlayerAbilities, CPlayerInfoUpdate, PlayerInfoEntry,
};
use rustyline::completion::Completer;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::FileHistory;
use rustyline::validate::Validator;
use rustyline::{Editor, Helper};
use rustyline::error::ReadlineError;
use time::{Duration, OffsetDateTime};
use tracing::{error, info};
use tracing::Subscriber;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use crate::server::Server;

const LOG_DIR: &str = "logs";
const LATEST_LOG: &str = "latest.log";
const MAX_LOG_ATTEMPTS: u32 = 1000;

/// 全局控制台状态（`init_logging` 创建；控制台线程取走/归还编辑器）。
static CONSOLE_STATE: OnceLock<ConsoleState> = OnceLock::new();

struct ConsoleState {
    readline: Mutex<Option<Editor<IcbmCommandCompleter, FileHistory>>>,
}

impl ConsoleState {
    fn take_readline(&self) -> Option<Editor<IcbmCommandCompleter, FileHistory>> {
        self.readline.lock().expect("readline lock").take()
    }

    fn return_readline(&self, rl: Editor<IcbmCommandCompleter, FileHistory>) {
        let mut slot = self.readline.lock().expect("readline lock");
        let _ = slot.insert(rl);
    }
}

/// 解析 `--log-level` 参数值。
pub fn parse_log_level(value: &str) -> Result<LevelFilter, String> {
    match value.to_ascii_lowercase().as_str() {
        "off" => Ok(LevelFilter::OFF),
        "error" => Ok(LevelFilter::ERROR),
        "warn" => Ok(LevelFilter::WARN),
        "info" => Ok(LevelFilter::INFO),
        "debug" => Ok(LevelFilter::DEBUG),
        "trace" => Ok(LevelFilter::TRACE),
        other => Err(format!("未知日志级别: {other}")),
    }
}

/// 初始化日志与控制台（在 `main` 中、模式分发前调用一次）。
///
/// 级别优先级：`RUST_LOG` 环境变量 > `--log-level` 参数（默认 INFO）。
/// `use_stderr=true`（region 子进程模式）时日志写 stderr、不创建交互编辑器。
pub fn init_logging(level: LevelFilter, use_stderr: bool) {
    use std::io::IsTerminal;
    use std::str::FromStr;

    let level = std::env::var("RUST_LOG")
        .ok()
        .as_deref()
        .map(LevelFilter::from_str)
        .and_then(Result::ok)
        .unwrap_or(level);

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level_filter_str(level)));

    let file_logger = GzipRollingLogger::new(level, LATEST_LOG.to_owned())
        .map_err(|err| eprintln!("failed to initialize file logger: {err}"))
        .ok();

    let (writer, readline): (
        Box<dyn Write + Send + 'static>,
        Option<Editor<IcbmCommandCompleter, FileHistory>>,
    ) = if use_stderr {
        (Box::new(io::stderr()), None)
    } else if io::stdin().is_terminal() {
        let rl_config = rustyline::Config::builder()
            .auto_add_history(true)
            .completion_type(rustyline::CompletionType::List)
            .edit_mode(rustyline::EditMode::Emacs)
            .build();
        match Editor::<IcbmCommandCompleter, FileHistory>::with_config(rl_config) {
            Ok(mut rl) => {
                rl.set_helper(Some(IcbmCommandCompleter::new()));
                (Box::new(io::stdout()), Some(rl))
            }
            Err(err) => {
                eprintln!(
                    "failed to initialize console input ({err}); falling back to simple logger"
                );
                (Box::new(io::stdout()), None)
            }
        }
    } else {
        (Box::new(io::stdout()), None)
    };

    let fmt_layer = fmt::layer()
        .with_writer(Mutex::new(writer))
        .with_ansi(true)
        .with_ansi_sanitization(false)
        .with_target(true)
        .with_timer(fmt::time::OffsetTime::new(
            time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC),
            time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]"),
        ));

    let registry = tracing_subscriber::registry().with(env_filter).with(fmt_layer);
    if let Some(file_logger) = file_logger {
        registry.with(file_logger).init();
    } else {
        registry.init();
    }

    assert!(
        CONSOLE_STATE
            .set(ConsoleState {
                readline: Mutex::new(readline),
            })
            .is_ok(),
        "logger already initialized"
    );
}

/// `LevelFilter` → 过滤字符串（`EnvFilter` 级别兜底 / `--log-level` 参数值）。
const fn level_filter_str(level: LevelFilter) -> &'static str {
    match level {
        LevelFilter::OFF => "off",
        LevelFilter::ERROR => "error",
        LevelFilter::WARN => "warn",
        LevelFilter::INFO => "info",
        LevelFilter::DEBUG => "debug",
        LevelFilter::TRACE => "trace",
    }
}

/// 启动控制台线程：优先 rustyline 交互控制台，否则逐行 stdin。
pub fn spawn_console(server: &Server) {
    let Some(state) = CONSOLE_STATE.get() else {
        return;
    };
    if let Some(rl) = state.take_readline() {
        run_readline_console(server.clone(), rl);
    } else {
        run_stdin_console(server.clone());
    }
}

/// rustyline 交互控制台线程（Ctrl-C 停止、Ctrl-D 停止；日志经 fmt 层直接写
/// stdout，rustyline 在下次输入时重绘提示行）。
fn run_readline_console(
    server: Server,
    mut rl: Editor<IcbmCommandCompleter, FileHistory>,
) {
    if let Some(helper) = rl.helper_mut() {
        helper.set_server(server.clone());
    }
    std::thread::Builder::new()
        .name("console-readline".to_owned())
        .spawn(move || {
            while server.running.load(Ordering::Relaxed) {
                match rl.readline("> ") {
                    Ok(line) => {
                        let _ = rl.add_history_entry(line.clone());
                        server.handle_command(&line);
                    }
                    Err(ReadlineError::Interrupted) => {
                        info!("CTRL-C");
                        server.stop();
                        break;
                    }
                    Err(ReadlineError::Eof) => {
                        info!("CTRL-D");
                        server.stop();
                        break;
                    }
                    Err(err) => {
                        error!("console read error: {err}");
                        break;
                    }
                }
            }
            if let Some(state) = CONSOLE_STATE.get() {
                state.return_readline(rl);
            }
        })
        .expect("spawn readline console thread");
}

/// 非 TTY 时的逐行 stdin 控制台线程。
fn run_stdin_console(server: Server) {
    std::thread::Builder::new()
        .name("console-stdin".to_owned())
        .spawn(move || {
            for line in io::stdin().lines() {
                let Ok(line) = line else { break };
                if !server.running.load(Ordering::Relaxed) {
                    break;
                }
                server.handle_command(&line);
            }
        })
        .expect("spawn stdin console thread");
}

/// 控制台命令名（升序；补全与提示用）。
const CONSOLE_COMMANDS: &[&str] = &[
    "about", "bmtool", "gamemode", "gamerule", "help", "list", "regions", "say", "stop",
];

/// rustyline 补全/高亮/提示助手（参考 master `IcbmCommandCompleter`）。
#[derive(Clone, Default)]
pub struct IcbmCommandCompleter {
    server: Arc<std::sync::RwLock<Option<Server>>>,
}

impl IcbmCommandCompleter {
    /// 新建助手（服务器在控制台线程启动时注入）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            server: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    fn set_server(&self, server: Server) {
        if let Ok(mut slot) = self.server.write() {
            *slot = Some(server);
        }
    }

    fn server(&self) -> Option<Server> {
        self.server.read().ok().and_then(|guard| (*guard).clone())
    }
}

impl Helper for IcbmCommandCompleter {}

impl Highlighter for IcbmCommandCompleter {
    fn highlight<'l>(&self, line: &'l str, _pos: usize) -> Cow<'l, str> {
        line.find(' ').map_or_else(
            || Cow::Owned(format!("\x1b[1;36m{line}\x1b[0m")),
            |first_space| {
                let (cmd, args) = line.split_at(first_space);
                Cow::Owned(format!("\x1b[1;36m{cmd}\x1b[0m{args}"))
            },
        )
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[90m{hint}\x1b[0m"))
    }
}

impl Hinter for IcbmCommandCompleter {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &rustyline::Context<'_>) -> Option<Self::Hint> {
        if line.is_empty() || pos < line.len() {
            return None;
        }
        if let Ok((_, candidates)) = self.complete(line, pos, ctx)
            && let Some(first) = candidates.first()
        {
            if first.starts_with('<') {
                return line.ends_with(' ').then(|| first.clone());
            }
            let last_word = line.split_whitespace().last().unwrap_or("");
            return first.strip_prefix(last_word).map(str::to_owned);
        }
        None
    }
}

impl Validator for IcbmCommandCompleter {}

impl Completer for IcbmCommandCompleter {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let cmd = &line[..pos];
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let ends_with_space = cmd.ends_with(' ');

        // 正在输入的段：尾部空格 → 空词；否则最后一个词（命令名算第 0 段）。
        let (context, typing): (Vec<&str>, &str) = if ends_with_space {
            (parts.clone(), "")
        } else if let Some((last, rest)) = parts.split_last() {
            (rest.to_vec(), last)
        } else {
            (Vec::new(), "")
        };

        let server = self.server();
        let mut candidates = match context.as_slice() {
            [] => CONSOLE_COMMANDS.iter().map(|name| (*name).to_owned()).collect(),
            ["gamemode"] => ["survival", "creative", "adventure", "spectator"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            ["gamemode", _] => server
                .as_ref()
                .map(player_names)
                .unwrap_or_default(),
            ["gamerule"] => icbm_gameplay_minecraft::gamerules::GAME_RULES
                .iter()
                .map(|rule| rule.name.to_owned())
                .collect(),
            ["gamerule", rule] => gamerule_values(rule),
            ["bmtool"] => vec!["tps".to_owned()],
            _ => Vec::new(),
        };
        candidates.retain(|candidate| candidate.starts_with(typing));
        candidates.sort();
        candidates.dedup();
        let start = cmd.rfind(' ').map_or(0, |i| i + 1);
        Ok((start, candidates))
    }
}

/// 在线玩家名（`gamemode <mode> <target>` 的第三段）。
fn player_names(server: &Server) -> Vec<String> {
    let world = server.world.lock().expect("world lock");
    world
        .players
        .values()
        .map(|player| player.name.lock().expect("name").clone())
        .collect()
}

/// `/gamerule <rule>` 的值建议：布尔规则给 true/false。
fn gamerule_values(rule: &str) -> Vec<String> {
    use icbm_gameplay_minecraft::gamerules::RuleType;
    match icbm_gameplay_minecraft::gamerules::find(rule) {
        Some(game_rule) if game_rule.rule_type == RuleType::Bool => {
            vec!["true".to_owned(), "false".to_owned()]
        }
        _ => Vec::new(),
    }
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
                    let (feedback, ok) = {
                        let mut world = self.world.lock().expect("world lock");
                        let feedback =
                            commands::apply_gamerule(&mut world.gamerules, &rule, value.as_deref());
                        let ok = feedback.is_ok();
                        (feedback.unwrap_or_else(|err| err), ok)
                    };
                    info!("{feedback}");
                    if ok {
                        self.persist_gamerules();
                    }
                }
                Command::Help => print_help(),
                Command::About => info!("{}", about_message()),
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
    info!("  about");
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

/// 按天 gzip 滚动的文件日志层（对照 master `GzipRollingLogger`）。
struct GzipRollingLoggerData {
    current_day_of_month: u8,
    last_rotate_time: OffsetDateTime,
    file: BufWriter<File>,
    latest_filename: String,
}

struct GzipRollingLogger {
    log_level: LevelFilter,
    data: Mutex<GzipRollingLoggerData>,
}

impl GzipRollingLogger {
    fn new(log_level: LevelFilter, filename: String) -> Result<Self, Box<dyn std::error::Error>> {
        let now = OffsetDateTime::now_utc();
        std::fs::create_dir_all(LOG_DIR)?;

        let latest_path = PathBuf::from(LOG_DIR).join(&filename);

        // 上次的 latest.log 滚成带日期的 .gz
        if latest_path.exists() {
            let new_gz_path = Self::new_filename(true)?;
            let mut file = File::open(&latest_path)?;
            let mut encoder = GzEncoder::new(
                BufWriter::new(File::create(&new_gz_path)?),
                flate2::Compression::best(),
            );
            io::copy(&mut file, &mut encoder)?;
            encoder.finish()?;
            std::fs::remove_file(&latest_path)?;
        }

        let file = BufWriter::new(File::create(&latest_path)?);
        Ok(Self {
            log_level,
            data: Mutex::new(GzipRollingLoggerData {
                current_day_of_month: now.day(),
                last_rotate_time: now,
                latest_filename: filename,
                file,
            }),
        })
    }

    fn new_filename(yesterday: bool) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let mut now = OffsetDateTime::now_utc();
        if yesterday {
            now -= Duration::days(1);
        }
        let date_format = format!("{}-{:02}-{:02}", now.year(), now.month() as u8, now.day());
        let log_path = PathBuf::from(LOG_DIR);

        let mut oldest_log = None;
        for id in 1..=MAX_LOG_ATTEMPTS {
            let filename = log_path.join(format!("{date_format}-{id}.log.gz"));
            if !filename.exists() {
                return Ok(filename);
            }
            let Ok(modified_time) = filename.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            if let Some((_, old_time)) = oldest_log {
                if modified_time < old_time {
                    oldest_log = Some((filename, modified_time));
                }
                continue;
            }
            oldest_log = Some((filename, modified_time));
        }
        if let Some((path, _)) = oldest_log {
            eprintln!(
                "Max log ids ({MAX_LOG_ATTEMPTS}) used for {date_format}; overwriting oldest log file: {}",
                path.display()
            );
            return Ok(path);
        }
        Err(format!(
            "Failed to find a unique log filename for date {date_format} after {MAX_LOG_ATTEMPTS} attempts.",
        )
        .into())
    }

    fn rotate_log(&self) -> Result<(), Box<dyn std::error::Error>> {
        let now = OffsetDateTime::now_utc();
        let mut data = self.data.lock().expect("log data lock");

        let new_gz_path = Self::new_filename(true)?;
        let latest_path = PathBuf::from(LOG_DIR).join(&data.latest_filename);

        data.file.flush()?;
        drop(std::mem::replace(
            &mut data.file,
            BufWriter::new(File::create("/dev/null")?),
        ));

        let mut file = File::open(&latest_path)?;
        let mut encoder = GzEncoder::new(
            BufWriter::new(File::create(&new_gz_path)?),
            flate2::Compression::best(),
        );
        io::copy(&mut file, &mut encoder)?;
        encoder.finish()?;

        data.current_day_of_month = now.day();
        data.last_rotate_time = now;
        data.file = BufWriter::new(File::create(&latest_path)?);
        Ok(())
    }
}

/// 去掉消息中的 ANSI 颜色码（文件日志保留纯文本）。
fn remove_ansi_color_code(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            for c_seq in it.by_ref() {
                if c_seq.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

impl<S> Layer<S> for GzipRollingLogger
where
    S: Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let level = metadata.level();

        let should_log = match *level {
            tracing::Level::ERROR => self.log_level >= LevelFilter::ERROR,
            tracing::Level::WARN => self.log_level >= LevelFilter::WARN,
            tracing::Level::INFO => self.log_level >= LevelFilter::INFO,
            tracing::Level::DEBUG => self.log_level >= LevelFilter::DEBUG,
            tracing::Level::TRACE => self.log_level >= LevelFilter::TRACE,
        };
        if !should_log {
            return;
        }

        let now = OffsetDateTime::now_utc();
        if let Ok(mut data) = self.data.lock() {
            let mut visitor = StringVisitor::default();
            event.record(&mut visitor);
            let message = visitor.0;
            let clean_message = remove_ansi_color_code(&message);

            let _ = writeln!(data.file, "[{level}] {clean_message}");
            let _ = data.file.flush();

            if data.current_day_of_month != now.day() {
                drop(data);
                if let Err(err) = self.rotate_log() {
                    eprintln!("failed to rotate log: {err}");
                }
            }
        }
    }
}

/// 提取事件消息字段（`tracing::field::Visit`）。
#[derive(Default)]
struct StringVisitor(String);

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
            if self.0.starts_with('"') && self.0.ends_with('"') {
                self.0 = self.0[1..self.0.len() - 1].to_string();
            }
        }
    }
}
