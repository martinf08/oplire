//! Rust implementation of Oplire's explicitly managed proxy pool.
//!
//! Rate limits are observations only: neither the watcher nor any automatic
//! command changes proxies or retries traffic in response to HTTP 429/quota.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{Duration as ChronoDuration, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use percent_encoding::percent_decode_str;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use std::{
    env, fs,
    io::{self, BufRead, BufReader, Seek, SeekFrom, Write},
    net::TcpStream as StdTcpStream,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::sleep,
};
use url::Url;

const LOCAL_HOST: &str = "127.0.0.1";
const LOCAL_PORT: u16 = 8899;
const DEFAULT_COOLDOWN_SECS: i64 = 600;
const DEFAULT_DEAD_AFTER: i64 = 3;
const FRESHNESS_SECS: i64 = 1800;
const HEADER_LIMIT: usize = 131_072;
const TRACE_URL: &str = "https://www.cloudflare.com/cdn-cgi/trace";

#[derive(Clone, Debug)]
struct Paths {
    state_dir: PathBuf,
    db: PathBuf,
    env_file: PathBuf,
    profile_file: PathBuf,
    run_dir: PathBuf,
    log_file: PathBuf,
}

impl Paths {
    fn from_env() -> Self {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let state_dir = env::var_os("OPLIRE_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if env::var_os("HOME").is_some_and(|home| home == std::ffi::OsStr::new("/root")) {
                    PathBuf::from("/root")
                } else if let Some(xdg) = env::var_os("XDG_DATA_HOME") {
                    PathBuf::from(xdg).join("oplire")
                } else {
                    home.join(".local/share/oplire")
                }
            });
        let default_log = home.join(".local/share/opencode/log/opencode.log");
        Self {
            db: env::var_os("OPLIRE_DB")
                .map(PathBuf::from)
                .unwrap_or_else(|| state_dir.join("proxies.db")),
            state_dir,
            env_file: env::var_os("OPLIRE_ENV_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/etc/opencode-proxy.env")),
            profile_file: env::var_os("OPLIRE_PROFILE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/etc/profile.d/99-opencode-proxy.sh")),
            run_dir: env::var_os("OPLIRE_RUN_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/run/proxy-rotate")),
            log_file: env::var_os("PROXY_ROTATE_LOG")
                .or_else(|| env::var_os("OPENCODE_LOG"))
                .map(PathBuf::from)
                .unwrap_or(default_log),
        }
    }

    fn db_parent(&self) -> &Path {
        self.db.parent().unwrap_or_else(|| Path::new("."))
    }
}

#[derive(Debug, Clone)]
struct Proxy {
    id: i64,
    label: Option<String>,
    scheme: String,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    country: Option<String>,
    status: String,
    fail_count: i64,
    ok_count: i64,
    last_used: Option<String>,
    last_ok: Option<String>,
    cooldown_until: Option<String>,
    note: Option<String>,
    source: Option<String>,
    last_seen: Option<String>,
    created_at: String,
}

#[derive(Serialize)]
struct ProxyExport<'a> {
    id: i64,
    label: &'a Option<String>,
    scheme: &'a str,
    host: &'a str,
    port: u16,
    username: Option<&'a str>,
    password: Option<&'static str>,
    country: &'a Option<String>,
    status: &'a str,
    fail_count: i64,
    ok_count: i64,
    last_used: &'a Option<String>,
    last_ok: &'a Option<String>,
    cooldown_until: &'a Option<String>,
    note: &'a Option<String>,
    source: &'a Option<String>,
    last_seen: &'a Option<String>,
    created_at: &'a str,
}

impl Proxy {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            label: row.get("label")?,
            scheme: row.get("scheme")?,
            host: row.get("host")?,
            port: row.get("port")?,
            username: row.get("username")?,
            password: row.get("password")?,
            country: row.get("country")?,
            status: row.get("status")?,
            fail_count: row.get("fail_count")?,
            ok_count: row.get("ok_count")?,
            last_used: row.get("last_used")?,
            last_ok: row.get("last_ok")?,
            cooldown_until: row.get("cooldown_until")?,
            note: row.get("note")?,
            source: row.get("source")?,
            last_seen: row.get("last_seen")?,
            created_at: row.get("created_at")?,
        })
    }

    fn url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let auth = self
            .username
            .as_ref()
            .map(|username| {
                let password = self.password.as_deref().unwrap_or_default();
                format!("{}:{}@", percent_encode(username), percent_encode(password))
            })
            .unwrap_or_default();
        format!("{}://{}{}:{}", self.scheme, auth, host, self.port)
    }

    fn display_url(&self) -> String {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let auth = self
            .username
            .as_ref()
            .map(|_| "***@".to_owned())
            .unwrap_or_default();
        format!("{}://{}{}:{}", self.scheme, auth, host, self.port)
    }

    fn export(&self) -> ProxyExport<'_> {
        ProxyExport {
            id: self.id,
            label: &self.label,
            scheme: &self.scheme,
            host: &self.host,
            port: self.port,
            username: self.username.as_deref(),
            password: self.password.as_ref().map(|_| "[REDACTED]"),
            country: &self.country,
            status: &self.status,
            fail_count: self.fail_count,
            ok_count: self.ok_count,
            last_used: &self.last_used,
            last_ok: &self.last_ok,
            cooldown_until: &self.cooldown_until,
            note: &self.note,
            source: &self.source,
            last_seen: &self.last_seen,
            created_at: &self.created_at,
        }
    }
}

fn percent_encode(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

#[derive(Debug)]
struct ProxyInput {
    scheme: String,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
}

fn parse_proxy(value: &str) -> Result<ProxyInput> {
    let value = value.trim();
    let raw = if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    };
    let url = Url::parse(&raw).map_err(|_| anyhow!("invalid proxy URL"))?;
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        bail!("proxy URL must not contain a path, query, or fragment");
    }
    let scheme = url.scheme().to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "socks5" | "socks5h") {
        bail!("unsupported proxy scheme {scheme}; use http:// or socks5://");
    }
    let host = url
        .host_str()
        .context("proxy URL has no host")?
        .trim_matches(['[', ']'])
        .to_owned();
    let default_port = if scheme.starts_with("socks") {
        1080
    } else {
        8080
    };
    let port = url.port().unwrap_or(default_port);
    let username = (!url.username().is_empty()).then(|| {
        percent_decode_str(url.username())
            .decode_utf8_lossy()
            .into_owned()
    });
    let password = url.password().map(|password| {
        percent_decode_str(password)
            .decode_utf8_lossy()
            .into_owned()
    });
    Ok(ProxyInput {
        scheme,
        host,
        port,
        username,
        password,
    })
}

fn now() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn timestamp_after(seconds: i64) -> String {
    (Utc::now() + ChronoDuration::seconds(seconds))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn timestamp_before(seconds: i64) -> String {
    (Utc::now() - ChronoDuration::seconds(seconds))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn init_database(paths: &Paths) -> Result<Connection> {
    fs::create_dir_all(paths.db_parent())?;
    let conn =
        Connection::open(&paths.db).with_context(|| format!("open {}", paths.db.display()))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE IF NOT EXISTS proxies (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             label TEXT, scheme TEXT NOT NULL DEFAULT 'http', host TEXT NOT NULL,
             port INTEGER NOT NULL, username TEXT, password TEXT, country TEXT,
             status TEXT NOT NULL DEFAULT 'active', fail_count INTEGER NOT NULL DEFAULT 0,
             ok_count INTEGER NOT NULL DEFAULT 0, last_used TEXT, last_ok TEXT,
             cooldown_until TEXT, note TEXT, source TEXT, last_seen TEXT,
             created_at TEXT NOT NULL,
             UNIQUE (scheme, host, port, username)
         );
         CREATE TABLE IF NOT EXISTS events (
             id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL,
             proxy_id INTEGER REFERENCES proxies(id) ON DELETE CASCADE,
             event TEXT NOT NULL, detail TEXT
         );
         CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT);
         INSERT OR IGNORE INTO settings(key,value) VALUES
             ('cooldown_seconds','600'), ('dead_after','3'), ('mode','direct'),
             ('freshness_seconds','1800');",
    )?;
    migrate_database(&conn)?;
    set_private_file(&paths.db)?;
    fs::create_dir_all(&paths.run_dir).ok();
    Ok(conn)
}

fn open_database(paths: &Paths, create: bool) -> Result<Connection> {
    if !paths.db.exists() {
        if !create {
            bail!("database not found; run `proxy-rotate init`");
        }
        return init_database(paths);
    }
    let conn =
        Connection::open(&paths.db).with_context(|| format!("open {}", paths.db.display()))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    migrate_database(&conn)?;
    Ok(conn)
}

fn migrate_database(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(proxies)")?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "last_seen") {
        conn.execute("ALTER TABLE proxies ADD COLUMN last_seen TEXT", [])?;
    }
    if !columns.iter().any(|column| column == "source") {
        conn.execute("ALTER TABLE proxies ADD COLUMN source TEXT", [])?;
    }
    conn.execute_batch(
        "INSERT OR IGNORE INTO settings(key,value) VALUES
         ('cooldown_seconds','600'), ('dead_after','3'), ('mode','direct'),
         ('freshness_seconds','1800');",
    )?;
    Ok(())
}

fn set_private_file(path: &Path) -> Result<()> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn get_setting(conn: &Connection, key: &str, default: &str) -> Result<String> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()?
        .unwrap_or_else(|| default.to_owned()))
}

fn set_setting(conn: &Connection, key: &str, value: impl ToString) -> Result<()> {
    conn.execute(
        "INSERT INTO settings(key,value) VALUES (?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(())
}

fn log_event(
    conn: &Connection,
    proxy_id: Option<i64>,
    event: &str,
    detail: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO events(ts,proxy_id,event,detail) VALUES (?1,?2,?3,?4)",
        params![now(), proxy_id, event, detail],
    )?;
    Ok(())
}

fn get_proxy(conn: &Connection, id: i64) -> Result<Proxy> {
    conn.query_row("SELECT * FROM proxies WHERE id=?1", [id], Proxy::from_row)
        .optional()?
        .ok_or_else(|| anyhow!("proxy #{id} not found"))
}

fn list_proxies(conn: &Connection) -> Result<Vec<Proxy>> {
    let mut stmt = conn.prepare("SELECT * FROM proxies ORDER BY id")?;
    let rows = stmt.query_map([], Proxy::from_row)?;
    let proxies = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(proxies)
}

fn parse_jobs(value: &str) -> std::result::Result<usize, String> {
    let jobs = value
        .parse::<usize>()
        .map_err(|_| "jobs must be an integer".to_owned())?;
    if (1..=100).contains(&jobs) {
        Ok(jobs)
    } else {
        Err("jobs must be between 1 and 100".to_owned())
    }
}

fn parse_probe_timeout(value: &str) -> std::result::Result<u64, String> {
    let timeout = value
        .parse::<u64>()
        .map_err(|_| "timeout must be an integer".to_owned())?;
    if (1..=120).contains(&timeout) {
        Ok(timeout)
    } else {
        Err("timeout must be between 1 and 120 seconds".to_owned())
    }
}

fn insert_proxy(
    conn: &Connection,
    input: &ProxyInput,
    label: Option<&str>,
    country: Option<&str>,
    note: Option<&str>,
    source: Option<&str>,
    seen: bool,
) -> Result<i64> {
    // SQLite UNIQUE permits multiple NULL usernames; compare them explicitly
    // without rewriting or deleting entries in existing user databases.
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM proxies WHERE scheme=?1 AND host=?2 AND port=?3 AND COALESCE(username,'')=COALESCE(?4,''))",
        params![input.scheme, input.host, input.port, input.username],
        |row| row.get(0),
    )?;
    if exists {
        bail!("UNIQUE constraint failed: proxy already exists");
    }
    let stamp = now();
    conn.execute(
        "INSERT INTO proxies(label,scheme,host,port,username,password,country,note,source,last_seen,created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![label, input.scheme, input.host, input.port, input.username, input.password,
            country, note, source, seen.then_some(stamp.as_str()), stamp],
    )?;
    let id = conn.last_insert_rowid();
    log_event(conn, Some(id), "add", source)?;
    Ok(id)
}

fn is_rate_limit_reason(reason: &str) -> bool {
    let lower = reason.to_ascii_lowercase();
    lower.contains("quota")
        || lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate-limit")
        || lower.contains("ratelimit")
        || lower.contains("quotaexceeded")
        || lower.contains("free usage limit")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_env(paths: &Paths, proxy: &str, comment: &str) -> Result<()> {
    let no_proxy =
        env::var("OPLIRE_NO_PROXY").unwrap_or_else(|_| "localhost,127.0.0.1,::1".to_owned());
    let text = format!(
        "# {comment}\nHTTPS_PROXY={proxy}\nHTTP_PROXY={proxy}\nALL_PROXY={proxy}\nhttps_proxy={proxy}\nhttp_proxy={proxy}\nall_proxy={proxy}\nNO_PROXY={no_proxy}\nno_proxy={no_proxy}\n",
        proxy = shell_quote(proxy),
        no_proxy = shell_quote(&no_proxy),
    );
    if let Some(parent) = paths.env_file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&paths.env_file, text)?;
    set_private_file(&paths.env_file)?;
    if let Some(parent) = paths.profile_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let profile = format!(
        "# Loaded at login: Oplire managed proxy\n[ -f {env_file} ] && set -a && . {env_file} && set +a\n",
        env_file = shell_quote(&paths.env_file.to_string_lossy()),
    );
    fs::write(&paths.profile_file, profile)?;
    Ok(())
}

fn apply_direct(paths: &Paths, conn: &Connection, proxy: &Proxy) -> Result<()> {
    let stamp = now();
    write_env(
        paths,
        &proxy.url(),
        &format!(
            "Generated by proxy-rotate at {stamp} for proxy #{}",
            proxy.id
        ),
    )?;
    conn.execute(
        "UPDATE proxies SET last_used=?1 WHERE id=?2",
        params![stamp, proxy.id],
    )?;
    set_setting(conn, "mode", "direct")?;
    set_setting(conn, "current_id", proxy.id)?;
    log_event(conn, Some(proxy.id), "use", Some("direct"))?;
    println!("Active proxy: #{} {}", proxy.id, proxy.display_url());
    println!("Written to {}", paths.env_file.display());
    println!("Restart OpenCode to apply it; source the generated environment file.");
    Ok(())
}

fn apply_local(paths: &Paths, conn: &Connection) -> Result<()> {
    write_env(
        paths,
        &format!("http://{LOCAL_HOST}:{LOCAL_PORT}"),
        "Generated by proxy-rotate in local daemon mode",
    )?;
    set_setting(conn, "mode", "local")?;
    println!("Local mode: OpenCode → http://{LOCAL_HOST}:{LOCAL_PORT} → selected proxy");
    println!("Written to {}", paths.env_file.display());
    Ok(())
}

fn current_proxy_id(paths: &Paths, conn: &Connection) -> Result<Option<i64>> {
    if get_setting(conn, "mode", "direct")? == "local" {
        let value = get_setting(conn, "current_id", "")?;
        return Ok(value.parse().ok());
    }
    if let Ok(content) = fs::read_to_string(&paths.env_file) {
        for line in content.lines() {
            if let Some(id) = line
                .split_once("proxy #")
                .and_then(|(_, id)| id.split_whitespace().next())
            {
                if let Ok(id) = id.parse() {
                    return Ok(Some(id));
                }
            }
        }
        if let Some(value) = content
            .lines()
            .find_map(|line| line.strip_prefix("HTTPS_PROXY="))
        {
            if let Ok(parsed) = parse_proxy(value) {
                return Ok(conn.query_row(
                    "SELECT id FROM proxies WHERE scheme=?1 AND host=?2 AND port=?3 AND COALESCE(username,'')=COALESCE(?4,'')",
                    params![parsed.scheme, parsed.host, parsed.port, parsed.username],
                    |row| row.get(0),
                ).optional()?);
            }
        }
    }
    Ok(None)
}

fn eligible_proxies(
    conn: &Connection,
    exclude: Option<i64>,
    fresh_only: bool,
) -> Result<Vec<Proxy>> {
    let mut rows = list_proxies(conn)?;
    let stamp = now();
    let fresh_cutoff = timestamp_before(
        get_setting(conn, "freshness_seconds", &FRESHNESS_SECS.to_string())?
            .parse()
            .unwrap_or(FRESHNESS_SECS),
    );
    rows.retain(|proxy| {
        matches!(proxy.status.as_str(), "active" | "cooldown")
            && proxy.id != exclude.unwrap_or(-1)
            && proxy
                .cooldown_until
                .as_deref()
                .is_none_or(|until| until <= stamp.as_str())
            && (!fresh_only
                || proxy
                    .last_seen
                    .as_deref()
                    .is_some_and(|seen| seen >= fresh_cutoff.as_str()))
    });
    rows.sort_by(|a, b| {
        a.last_used
            .is_some()
            .cmp(&b.last_used.is_some())
            .then_with(|| a.last_used.cmp(&b.last_used))
            .then(a.id.cmp(&b.id))
    });
    Ok(rows)
}

fn pick_proxy(conn: &Connection, exclude: Option<i64>) -> Result<Option<Proxy>> {
    if let Some(proxy) = eligible_proxies(conn, exclude, true)?.into_iter().next() {
        return Ok(Some(proxy));
    }
    Ok(eligible_proxies(conn, exclude, false)?.into_iter().next())
}

fn apply_cooldown(conn: &Connection, proxy: &Proxy) -> Result<String> {
    let seconds = get_setting(conn, "cooldown_seconds", &DEFAULT_COOLDOWN_SECS.to_string())?
        .parse::<i64>()
        .unwrap_or(DEFAULT_COOLDOWN_SECS);
    let until = timestamp_after(seconds);
    conn.execute(
        "UPDATE proxies SET status='cooldown',cooldown_until=?1 WHERE id=?2",
        params![until, proxy.id],
    )?;
    Ok(until)
}

#[derive(Parser)]
#[command(
    name = "proxy-rotate",
    version,
    about = "Manage explicitly authorized proxies; HTTP 429 never changes routes or replays requests."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init,
    Add {
        url: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        country: Option<String>,
        #[arg(long)]
        note: Option<String>,
    },
    Import {
        file: PathBuf,
        #[arg(long, default_value = "import")]
        source: String,
    },
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    Remove {
        id: i64,
    },
    Enable {
        id: i64,
    },
    Disable {
        id: i64,
    },
    Use {
        id: i64,
    },
    Rotate {
        #[arg(long)]
        reason: Option<String>,
    },
    #[command(
        name = "429",
        about = "Report a provider limit without switching proxies."
    )]
    RateLimit,
    Ok {
        id: Option<i64>,
    },
    Fail {
        id: Option<i64>,
    },
    Test {
        id: Option<i64>,
        #[arg(long, conflicts_with = "id")]
        all: bool,
    },
    Refresh {
        #[arg(long, default_value_t=20, value_parser=parse_jobs)]
        jobs: usize,
        #[arg(long, default_value_t=8, value_parser=parse_probe_timeout)]
        timeout: u64,
    },
    Auto {
        #[arg(value_enum)]
        action: Option<AutoAction>,
    },
    Start,
    Stop,
    #[command(hide = true)]
    Daemon,
    #[command(hide = true)]
    Watch,
    Status,
    Reset,
    Prune,
    Clear,
    Export {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AutoAction {
    On,
    Off,
    Status,
}

pub async fn run_cli() -> Result<i32> {
    let cli = Cli::parse();
    let paths = Paths::from_env();
    match cli.command {
        Command::RateLimit => {
            println!("HTTP 429/quota: no proxy switch, WARP reset, or retry.");
            println!("Honor Retry-After and provider limits; the original request remains the only attempt.");
            Ok(0)
        }
        Command::Rotate {
            reason: Some(reason),
        } if is_rate_limit_reason(&reason) => {
            println!("Refusing: HTTP 429/quota must never trigger a proxy switch.");
            Ok(2)
        }
        Command::Daemon => {
            run_daemon(paths).await?;
            Ok(0)
        }
        Command::Watch => {
            run_watcher(paths).await?;
            Ok(0)
        }
        Command::Init => {
            let _conn = init_database(&paths)?;
            println!("Database ready: {}", paths.db.display());
            Ok(0)
        }
        command => {
            let conn = open_database(&paths, true)?;
            match command {
                Command::Add {
                    url,
                    label,
                    country,
                    note,
                } => cmd_add(
                    &conn,
                    &url,
                    label.as_deref(),
                    country.as_deref(),
                    note.as_deref(),
                )?,
                Command::Import { file, source } => cmd_import(&conn, &file, &source)?,
                Command::List { all, json } => cmd_list(&paths, &conn, all, json)?,
                Command::Remove { id } => cmd_remove(&conn, id)?,
                Command::Enable { id } => cmd_set_status(&conn, id, "active")?,
                Command::Disable { id } => cmd_set_status(&conn, id, "disabled")?,
                Command::Use { id } => cmd_use(&paths, &conn, id)?,
                Command::Rotate { reason } => return cmd_rotate(&paths, &conn, reason.as_deref()),
                Command::Ok { id } => cmd_mark_ok(&paths, &conn, id)?,
                Command::Fail { id } => cmd_mark_fail(&paths, &conn, id)?,
                Command::Test { id, all } => return cmd_test(&paths, &conn, id, all, 12).await,
                Command::Refresh { jobs, timeout } => {
                    return cmd_refresh(&conn, jobs, timeout).await
                }
                Command::Auto { action } => {
                    return cmd_auto(&paths, &conn, action.unwrap_or(AutoAction::Status)).await
                }
                Command::Start => {
                    start_services(&paths).await?;
                    println!("Daemon and watcher started.");
                }
                Command::Stop => {
                    stop_service(&paths, "watch")?;
                    stop_service(&paths, "daemon")?;
                    println!("Daemon and watcher stopped.");
                }
                Command::Status => cmd_status(&paths, &conn)?,
                Command::Reset => cmd_reset(&conn)?,
                Command::Prune => cmd_prune(&conn)?,
                Command::Clear => cmd_clear(&paths, &conn)?,
                Command::Export { json } => cmd_export(&conn, json)?,
                Command::Init | Command::RateLimit | Command::Daemon | Command::Watch => {
                    unreachable!()
                }
            }
            Ok(0)
        }
    }
}

fn cmd_add(
    conn: &Connection,
    url: &str,
    label: Option<&str>,
    country: Option<&str>,
    note: Option<&str>,
) -> Result<()> {
    let input = parse_proxy(url)?;
    let id = insert_proxy(conn, &input, label, country, note, None, false).map_err(|error| {
        if error.to_string().contains("UNIQUE constraint failed") {
            anyhow!("this proxy already exists")
        } else {
            error
        }
    })?;
    println!(
        "Added #{id}: {}://{}:{}",
        input.scheme, input.host, input.port
    );
    Ok(())
}

fn cmd_import(conn: &Connection, file: &Path, source: &str) -> Result<()> {
    let text = fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    let mut added = 0;
    let mut refreshed = 0;
    let mut invalid = 0;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let input = match parse_proxy(line) {
            Ok(value) => value,
            Err(_) => {
                invalid += 1;
                continue;
            }
        };
        match insert_proxy(conn, &input, None, None, None, Some(source), true) {
            Ok(_) => added += 1,
            Err(error) if error.to_string().contains("UNIQUE constraint failed") => {
                conn.execute("UPDATE proxies SET last_seen=?1,status=CASE WHEN status='dead' THEN 'active' ELSE status END WHERE scheme=?2 AND host=?3 AND port=?4 AND COALESCE(username,'')=COALESCE(?5,'')",
                    params![now(), input.scheme, input.host, input.port, input.username])?;
                refreshed += 1;
            }
            Err(error) => return Err(error),
        }
    }
    println!("Import: {added} added, {refreshed} refreshed, {invalid} invalid");
    Ok(())
}

fn cmd_list(paths: &Paths, conn: &Connection, all: bool, json: bool) -> Result<()> {
    let mut proxies = list_proxies(conn)?;
    if !all {
        proxies.retain(|proxy| proxy.status != "disabled");
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&proxies.iter().map(Proxy::export).collect::<Vec<_>>())?
        );
        return Ok(());
    }
    if proxies.is_empty() {
        println!("No proxies. Add one with: proxy-rotate add http://user:pass@host:port");
        return Ok(());
    }
    let current = current_proxy_id(paths, conn)?;
    let freshness = get_setting(conn, "freshness_seconds", &FRESHNESS_SECS.to_string())?
        .parse::<i64>()
        .unwrap_or(FRESHNESS_SECS);
    let cutoff = timestamp_before(freshness);
    println!("   ID  STATUS   COUNTRY SEEN             OK FAIL  URL");
    for proxy in proxies {
        let mark = if Some(proxy.id) == current {
            '*'
        } else if proxy
            .last_seen
            .as_deref()
            .is_some_and(|seen| seen >= cutoff.as_str())
        {
            ' '
        } else {
            '!'
        };
        let seen = proxy
            .last_seen
            .as_deref()
            .map(|s| s.chars().skip(5).take(11).collect::<String>())
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{mark} {:>4}  {:<8} {:<7} {:<16} {:>3} {:>4}  {}",
            proxy.id,
            proxy.status,
            proxy.country.as_deref().unwrap_or("-"),
            seen,
            proxy.ok_count,
            proxy.fail_count,
            proxy.display_url()
        );
    }
    Ok(())
}

fn cmd_remove(conn: &Connection, id: i64) -> Result<()> {
    get_proxy(conn, id)?;
    conn.execute("DELETE FROM proxies WHERE id=?1", [id])?;
    if get_setting(conn, "current_id", "")? == id.to_string() {
        set_setting(conn, "current_id", "")?;
    }
    println!("Removed #{id}");
    Ok(())
}

fn cmd_set_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    get_proxy(conn, id)?;
    conn.execute(
        "UPDATE proxies SET status=?1 WHERE id=?2",
        params![status, id],
    )?;
    log_event(conn, Some(id), status, None)?;
    println!("#{id} → {status}");
    Ok(())
}

fn cmd_use(paths: &Paths, conn: &Connection, id: i64) -> Result<()> {
    let proxy = get_proxy(conn, id)?;
    if proxy.status != "active" {
        bail!("proxy #{id} is not active ({})", proxy.status);
    }
    apply_direct(paths, conn, &proxy)
}

fn cmd_rotate(paths: &Paths, conn: &Connection, reason: Option<&str>) -> Result<i32> {
    if reason.is_some_and(is_rate_limit_reason) {
        println!("Refusing: HTTP 429/quota must never trigger a proxy switch.");
        return Ok(2);
    }
    let current = current_proxy_id(paths, conn)?;
    let Some(proxy) = pick_proxy(conn, current)? else {
        println!("No different proxy is available; network settings were not changed.");
        return Ok(1);
    };
    if get_setting(conn, "mode", "direct")? == "local" {
        conn.execute(
            "UPDATE proxies SET last_used=?1 WHERE id=?2",
            params![now(), proxy.id],
        )?;
        set_setting(conn, "current_id", proxy.id)?;
        log_event(
            conn,
            Some(proxy.id),
            "rotate",
            Some(reason.unwrap_or("manual (live switch)")),
        )?;
        println!(
            "Active proxy: #{} {} (live switch)",
            proxy.id,
            proxy.display_url()
        );
    } else {
        if let Some(reason) = reason {
            log_event(conn, Some(proxy.id), "rotate", Some(reason))?;
        }
        apply_direct(paths, conn, &proxy)?;
    }
    Ok(0)
}

fn cmd_mark_ok(paths: &Paths, conn: &Connection, id: Option<i64>) -> Result<()> {
    let id = id
        .or(current_proxy_id(paths, conn)?)
        .context("no current proxy; specify an ID")?;
    get_proxy(conn, id)?;
    let stamp = now();
    conn.execute("UPDATE proxies SET status='active',ok_count=ok_count+1,fail_count=0,last_ok=?1,last_seen=?1,cooldown_until=NULL WHERE id=?2", params![stamp, id])?;
    log_event(conn, Some(id), "ok", None)?;
    println!("#{id} OK");
    Ok(())
}

fn cmd_mark_fail(paths: &Paths, conn: &Connection, id: Option<i64>) -> Result<()> {
    let id = id
        .or(current_proxy_id(paths, conn)?)
        .context("no current proxy; specify an ID")?;
    let proxy = get_proxy(conn, id)?;
    let failures = proxy.fail_count + 1;
    let dead_after = get_setting(conn, "dead_after", &DEFAULT_DEAD_AFTER.to_string())?
        .parse::<i64>()
        .unwrap_or(DEFAULT_DEAD_AFTER);
    if failures >= dead_after {
        conn.execute(
            "UPDATE proxies SET status='dead',fail_count=?1 WHERE id=?2",
            params![failures, id],
        )?;
        log_event(
            conn,
            Some(id),
            "dead",
            Some(&format!("{failures} failures")),
        )?;
        println!("#{id} → dead ({failures} failures)");
    } else {
        let until = apply_cooldown(conn, &proxy)?;
        conn.execute(
            "UPDATE proxies SET fail_count=?1 WHERE id=?2",
            params![failures, id],
        )?;
        log_event(conn, Some(id), "fail", Some(&format!("failure {failures}")))?;
        println!("#{id} → cooldown until {until} UTC ({failures} failure(s))");
    }
    Ok(())
}

fn cmd_status(paths: &Paths, conn: &Connection) -> Result<()> {
    let proxies = list_proxies(conn)?;
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for proxy in &proxies {
        *counts.entry(proxy.status.clone()).or_default() += 1;
    }
    println!(
        "Database: {} — {} proxy/proxies",
        paths.db.display(),
        proxies.len()
    );
    for (status, count) in counts {
        println!("  {status:<9} {count:>3}");
    }
    let ttl = get_setting(conn, "freshness_seconds", &FRESHNESS_SECS.to_string())?
        .parse::<i64>()
        .unwrap_or(FRESHNESS_SECS);
    let cutoff = timestamp_before(ttl);
    let fresh = proxies
        .iter()
        .filter(|p| {
            p.status == "active" && p.last_seen.as_deref().is_some_and(|s| s >= cutoff.as_str())
        })
        .count();
    println!("  fresh     {fresh:>3}  (checked < {} min ago)", ttl / 60);
    println!("Mode     : {}", get_setting(conn, "mode", "direct")?);
    println!(
        "Daemon   : {} — watcher {}",
        if port_open() { "running" } else { "stopped" },
        if service_alive(&paths.run_dir, "watch") {
            "running"
        } else {
            "stopped"
        }
    );
    if let Some(id) = current_proxy_id(paths, conn)? {
        let proxy = get_proxy(conn, id)?;
        println!("Active   : #{id} {}", proxy.display_url());
    } else {
        println!("Active   : no proxy managed by Oplire");
    }
    println!(
        "Cooldown : {}s — mark dead after {} failures",
        get_setting(conn, "cooldown_seconds", "600")?,
        get_setting(conn, "dead_after", "3")?
    );
    Ok(())
}

fn cmd_reset(conn: &Connection) -> Result<()> {
    conn.execute("UPDATE proxies SET status='active',fail_count=0,cooldown_until=NULL WHERE status IN ('cooldown','dead')", [])?;
    log_event(conn, None, "reset", Some("cooldowns and counters cleared"))?;
    println!("Cooldowns and counters cleared.");
    Ok(())
}

fn cmd_prune(conn: &Connection) -> Result<()> {
    let removed = conn.execute("DELETE FROM proxies WHERE status='dead'", [])?;
    println!("Removed {removed} dead proxy/proxies.");
    Ok(())
}

fn cmd_clear(paths: &Paths, conn: &Connection) -> Result<()> {
    let mut removed = false;
    for path in [&paths.env_file, &paths.profile_file] {
        if path.exists() {
            fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
            removed = true;
        }
    }
    set_setting(conn, "mode", "direct")?;
    log_event(conn, None, "clear", None)?;
    println!(
        "{}",
        if removed {
            "Proxy removed from the environment."
        } else {
            "No proxy is currently applied."
        }
    );
    Ok(())
}

fn cmd_export(conn: &Connection, json: bool) -> Result<()> {
    let proxies = list_proxies(conn)?
        .into_iter()
        .filter(|proxy| proxy.status == "active")
        .collect::<Vec<_>>();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&proxies.iter().map(Proxy::export).collect::<Vec<_>>())?
        );
    } else {
        for proxy in proxies {
            println!("{}", proxy.url());
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct ProbeResult {
    status: Option<u16>,
    elapsed: Duration,
}

async fn probe_proxy(proxy: &Proxy, timeout: Duration) -> ProbeResult {
    let started = Instant::now();
    let status = async {
        let route = reqwest::Proxy::all(proxy.url())?;
        let client = reqwest::Client::builder()
            .proxy(route)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok::<u16, anyhow::Error>(client.get(TRACE_URL).send().await?.status().as_u16())
    }
    .await
    .ok();
    ProbeResult {
        status,
        elapsed: started.elapsed(),
    }
}

fn record_probe(
    conn: &Connection,
    proxy: &Proxy,
    probe: ProbeResult,
    event: &str,
    mark_used: bool,
) -> Result<bool> {
    let Some(status) = probe.status else {
        let failures = proxy.fail_count + 1;
        let dead_after = get_setting(conn, "dead_after", &DEFAULT_DEAD_AFTER.to_string())?
            .parse::<i64>()
            .unwrap_or(DEFAULT_DEAD_AFTER);
        if failures >= dead_after {
            conn.execute(
                "UPDATE proxies SET status='dead',fail_count=?1 WHERE id=?2",
                params![failures, proxy.id],
            )?;
            log_event(
                conn,
                Some(proxy.id),
                "dead",
                Some("connectivity probe failed"),
            )?;
        } else {
            let until = apply_cooldown(conn, proxy)?;
            conn.execute(
                "UPDATE proxies SET fail_count=?1 WHERE id=?2",
                params![failures, proxy.id],
            )?;
            log_event(
                conn,
                Some(proxy.id),
                event,
                Some(&format!(
                    "connectivity probe failed; cooldown until {until}"
                )),
            )?;
        }
        return Ok(false);
    };

    // Any HTTP response proves the route reached the endpoint. In particular,
    // HTTP 429 is not a connectivity failure and must never cause a switch.
    let stamp = now();
    if mark_used {
        conn.execute("UPDATE proxies SET status='active',ok_count=ok_count+1,fail_count=0,last_ok=?1,last_seen=?1,last_used=?1,cooldown_until=NULL WHERE id=?2", params![stamp, proxy.id])?;
    } else {
        conn.execute("UPDATE proxies SET status='active',ok_count=ok_count+1,fail_count=0,last_ok=?1,last_seen=?1,cooldown_until=NULL WHERE id=?2", params![stamp, proxy.id])?;
    }
    log_event(
        conn,
        Some(proxy.id),
        event,
        Some(&format!("HTTP {status}; {}ms", probe.elapsed.as_millis())),
    )?;
    Ok(true)
}

async fn cmd_test(
    paths: &Paths,
    conn: &Connection,
    id: Option<i64>,
    all: bool,
    timeout: u64,
) -> Result<i32> {
    let proxies = if all {
        list_proxies(conn)?
    } else {
        let id = id
            .or(current_proxy_id(paths, conn)?)
            .context("no current proxy; specify an ID or --all")?;
        vec![get_proxy(conn, id)?]
    };
    for proxy in proxies {
        let result = probe_proxy(&proxy, Duration::from_secs(timeout)).await;
        let connected = record_probe(conn, &proxy, result, "test", true)?;
        match (connected, result.status) {
            (true, Some(429)) => println!(
                "#{:<3} HTTP 429  {:5.2}s  connectivity OK; no proxy switch  {}",
                proxy.id,
                result.elapsed.as_secs_f64(),
                proxy.display_url()
            ),
            (true, Some(status)) => println!(
                "#{:<3} HTTP {status:<3} {:5.2}s  {}",
                proxy.id,
                result.elapsed.as_secs_f64(),
                proxy.display_url()
            ),
            _ => println!(
                "#{:<3} FAIL     {:5.2}s  {}",
                proxy.id,
                result.elapsed.as_secs_f64(),
                proxy.display_url()
            ),
        }
    }
    Ok(0)
}

async fn cmd_refresh(conn: &Connection, jobs: usize, timeout: u64) -> Result<i32> {
    use futures_util::StreamExt;
    let proxies = list_proxies(conn)?
        .into_iter()
        .filter(|proxy| proxy.status != "disabled")
        .collect::<Vec<_>>();
    if proxies.is_empty() {
        println!("No proxies to refresh.");
        return Ok(0);
    }
    let results = futures_util::stream::iter(proxies)
        .map(|proxy| async move {
            let result = probe_proxy(&proxy, Duration::from_secs(timeout)).await;
            (proxy, result)
        })
        .buffer_unordered(jobs)
        .collect::<Vec<_>>()
        .await;
    let mut connected = 0;
    let mut failed = 0;
    for (proxy, result) in results {
        if record_probe(conn, &proxy, result, "refresh", false)? {
            connected += 1;
        } else {
            failed += 1;
        }
    }
    log_event(
        conn,
        None,
        "refresh",
        Some(&format!("{connected} connected / {failed} failed")),
    )?;
    println!("Refresh: {connected} connected, {failed} unreachable ({} tested); HTTP status codes never trigger a route change.", connected + failed);
    Ok(0)
}

async fn cmd_auto(paths: &Paths, conn: &Connection, action: AutoAction) -> Result<i32> {
    match action {
        AutoAction::On => {
            if current_proxy_id(paths, conn)?.is_none() {
                let Some(proxy) = pick_proxy(conn, None)? else {
                    println!("No active proxy; automatic mode was not enabled.");
                    return Ok(1);
                };
                set_setting(conn, "current_id", proxy.id)?;
                conn.execute(
                    "UPDATE proxies SET last_used=?1 WHERE id=?2",
                    params![now(), proxy.id],
                )?;
            }
            apply_local(paths, conn)?;
            start_services(paths).await?;
            println!("Automatic mode enabled (local daemon + connectivity/limit observer).");
            println!("HTTP 429 is informational and never changes proxy selection.");
            Ok(0)
        }
        AutoAction::Off => {
            stop_service(paths, "watch")?;
            stop_service(paths, "daemon")?;
            set_setting(conn, "mode", "direct")?;
            if paths.env_file.exists() {
                fs::remove_file(&paths.env_file)?;
            }
            println!("Automatic mode disabled (direct connection; WARP was not changed).");
            Ok(0)
        }
        AutoAction::Status => {
            let mode = get_setting(conn, "mode", "direct")?;
            println!("Mode       : {mode}");
            println!(
                "Daemon     : {} (http://{LOCAL_HOST}:{LOCAL_PORT})",
                if port_open() { "running" } else { "stopped" }
            );
            println!(
                "Watcher    : {}",
                if service_alive(&paths.run_dir, "watch") {
                    "running"
                } else {
                    "stopped"
                }
            );
            if let Some(id) = current_proxy_id(paths, conn)? {
                println!("Current    : #{id} {}", get_proxy(conn, id)?.display_url());
            } else {
                println!("Current    : none");
            }
            if mode == "local" {
                println!(
                    "Environment: {} → http://{LOCAL_HOST}:{LOCAL_PORT}",
                    paths.env_file.display()
                );
            }
            Ok(0)
        }
    }
}

async fn start_services(paths: &Paths) -> Result<()> {
    ensure_service(paths, "daemon").await?;
    ensure_service(paths, "watch").await?;
    Ok(())
}

async fn ensure_service(paths: &Paths, name: &str) -> Result<()> {
    if name == "daemon" && port_open() {
        return Ok(());
    }
    if service_alive(&paths.run_dir, name) {
        return Ok(());
    }
    fs::create_dir_all(&paths.run_dir)?;
    let log_path = paths.run_dir.join(format!("{name}.log"));
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    let log_err = log.try_clone()?;
    let executable = env::current_exe()?;
    ProcessCommand::new(executable)
        .arg(name)
        .env("OPLIRE_STATE_DIR", &paths.state_dir)
        .env("OPLIRE_DB", &paths.db)
        .env("OPLIRE_ENV_FILE", &paths.env_file)
        .env("OPLIRE_PROFILE_FILE", &paths.profile_file)
        .env("OPLIRE_RUN_DIR", &paths.run_dir)
        .env("OPENCODE_LOG", &paths.log_file)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()
        .with_context(|| format!("start proxy-rotate {name}"))?;
    for _ in 0..30 {
        if (name == "daemon" && port_open()) || service_alive(&paths.run_dir, name) {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "proxy-rotate {name} did not start; inspect {}",
        paths.run_dir.join(format!("{name}.log")).display()
    )
}

fn pid_path(run_dir: &Path, name: &str) -> PathBuf {
    run_dir.join(format!("{name}.pid"))
}

fn read_pid(run_dir: &Path, name: &str) -> Option<u32> {
    fs::read_to_string(pid_path(run_dir, name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        ProcessCommand::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
    #[cfg(windows)]
    {
        ProcessCommand::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
    }
}

fn service_alive(run_dir: &Path, name: &str) -> bool {
    read_pid(run_dir, name).is_some_and(process_alive)
}

fn stop_service(paths: &Paths, name: &str) -> Result<bool> {
    let Some(pid) = read_pid(&paths.run_dir, name) else {
        return Ok(false);
    };
    #[cfg(unix)]
    let _ = ProcessCommand::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status();
    #[cfg(windows)]
    let _ = ProcessCommand::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T"])
        .status();
    for _ in 0..20 {
        if !process_alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if process_alive(pid) {
        #[cfg(unix)]
        let _ = ProcessCommand::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
        #[cfg(windows)]
        let _ = ProcessCommand::new("taskkill")
            .args(["/F", "/PID", &pid.to_string(), "/T"])
            .status();
    }
    let _ = fs::remove_file(pid_path(&paths.run_dir, name));
    Ok(true)
}

fn port_open() -> bool {
    StdTcpStream::connect_timeout(
        &format!("{LOCAL_HOST}:{LOCAL_PORT}").parse().unwrap(),
        Duration::from_millis(300),
    )
    .is_ok()
}

struct PidGuard {
    pid_file: PathBuf,
    lock_file: Option<PathBuf>,
}

impl PidGuard {
    fn create(paths: &Paths, name: &str, lock: bool) -> Result<Self> {
        fs::create_dir_all(&paths.run_dir)?;
        let pid_file = pid_path(&paths.run_dir, name);
        let lock_file = lock.then(|| paths.run_dir.join(format!("{name}.lock")));
        if let Some(lock_file) = &lock_file {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_file)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    if service_alive(&paths.run_dir, name) {
                        bail!("watcher is already running");
                    }
                    fs::remove_file(lock_file)?;
                    let mut file = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(lock_file)?;
                    writeln!(file, "{}", std::process::id())?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        fs::write(&pid_file, std::process::id().to_string())?;
        Ok(Self {
            pid_file,
            lock_file,
        })
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.pid_file);
        if let Some(path) = &self.lock_file {
            let _ = fs::remove_file(path);
        }
    }
}

async fn run_daemon(paths: Paths) -> Result<()> {
    let _pid = PidGuard::create(&paths, "daemon", false)?;
    let listener = TcpListener::bind((LOCAL_HOST, LOCAL_PORT))
        .await
        .with_context(|| format!("bind local proxy on {LOCAL_HOST}:{LOCAL_PORT}"))?;
    println!("proxy-rotate local proxy listening on {LOCAL_HOST}:{LOCAL_PORT}");
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (client, _) = accepted?;
                let paths = paths.clone();
                tokio::spawn(async move {
                    if let Err(error) = serve_client(client, &paths).await {
                        tracing::debug!(%error, "local proxy connection closed");
                    }
                });
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                break;
            }
        }
    }
    Ok(())
}

fn current_upstream(paths: &Paths) -> Result<Option<Proxy>> {
    let conn = open_database(paths, false)?;
    if get_setting(&conn, "mode", "direct")? != "local" {
        return Ok(None);
    }
    let id = get_setting(&conn, "current_id", "")?
        .parse::<i64>()
        .context("no selected proxy in local mode")?;
    let proxy = get_proxy(&conn, id)?;
    if !matches!(proxy.status.as_str(), "active" | "cooldown") {
        bail!("selected proxy is not available");
    }
    Ok(Some(proxy))
}

async fn serve_client(mut client: TcpStream, paths: &Paths) -> Result<()> {
    let (head, rest) =
        match tokio::time::timeout(Duration::from_secs(30), read_http_head(&mut client)).await {
            Ok(result) => result.context("read client request")?,
            Err(_) => bail!("timed out reading client request headers"),
        };
    // Client credentials belong to this proxy, never to the destination or to
    // the selected upstream proxy (which has its own configured credentials).
    let head = strip_proxy_authorization(&head);
    let first_line = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let request_line = String::from_utf8_lossy(first_line)
        .trim_end_matches('\r')
        .to_owned();
    let mut fields = request_line.split_whitespace();
    let method = fields.next().context("invalid HTTP request line")?;
    let target = fields.next().context("invalid HTTP request line")?;
    let version = fields.next().unwrap_or("HTTP/1.1");
    let upstream = match current_upstream(paths) {
        Ok(upstream) => upstream,
        Err(error) => {
            client.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
            return Err(error);
        }
    };

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = parse_authority(target)?;
        let (mut remote, pending) = match upstream.as_ref() {
            Some(proxy) if proxy.scheme.starts_with("socks") => {
                socks_connect(proxy, &host, port).await?
            }
            Some(proxy) => http_connect(proxy, &host, port).await?,
            None => (connect_tcp(&host, port).await?, Vec::new()),
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        if !pending.is_empty() {
            client.write_all(&pending).await?;
        }
        if !rest.is_empty() {
            remote.write_all(&rest).await?;
        }
        tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
        return Ok(());
    }

    let url = Url::parse(target).context("plain HTTP requests must use absolute-form targets")?;
    if url.scheme() != "http" {
        client
            .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
            .await?;
        bail!("non-CONNECT proxy requests must use http:// targets");
    }
    let host = url
        .host_str()
        .context("request URL has no host")?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = url.port().unwrap_or(80);

    if let Some(proxy) = upstream.as_ref().filter(|proxy| proxy.scheme == "http") {
        let mut remote = connect_tcp(&proxy.host, proxy.port).await?;
        let auth = proxy_authorization(proxy);
        let outgoing = add_header(&head, "Proxy-Authorization", &auth);
        remote.write_all(&outgoing).await?;
        if !rest.is_empty() {
            remote.write_all(&rest).await?;
        }
        tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
        return Ok(());
    }

    let mut remote = match upstream.as_ref() {
        Some(proxy) => socks_connect(proxy, &host, port).await?.0,
        None => connect_tcp(&host, port).await?,
    };
    let mut path = if url.path().is_empty() {
        "/".to_owned()
    } else {
        url.path().to_owned()
    };
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let outgoing = rewrite_request_line(&head, &format!("{method} {path} {version}"));
    remote.write_all(&outgoing).await?;
    if !rest.is_empty() {
        remote.write_all(&rest).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut remote).await?;
    Ok(())
}

async fn connect_tcp(host: &str, port: u16) -> Result<TcpStream> {
    let stream = tokio::time::timeout(Duration::from_secs(15), TcpStream::connect((host, port)))
        .await
        .context("connection timed out")??;
    Ok(stream)
}

async fn read_http_head(stream: &mut TcpStream) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before HTTP headers",
            ));
        }
        data.extend_from_slice(&chunk[..read]);
        if let Some(index) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            if index + 4 > HEADER_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP headers too large",
                ));
            }
            let rest = data.split_off(index + 4);
            return Ok((data, rest));
        }
        if data.len() > HEADER_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers too large",
            ));
        }
    }
}

fn parse_authority(authority: &str) -> Result<(String, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest.find(']').context("invalid bracketed host")?;
        let host = rest[..end].to_owned();
        let port = rest[end + 1..]
            .strip_prefix(':')
            .context("missing port")?
            .parse::<u16>()?;
        return Ok((host, port));
    }
    let (host, port) = authority
        .rsplit_once(':')
        .context("authority must include a port")?;
    Ok((host.to_owned(), port.parse::<u16>()?))
}

fn proxy_authorization(proxy: &Proxy) -> String {
    match &proxy.username {
        Some(username) => {
            let value = format!(
                "{username}:{}",
                proxy.password.as_deref().unwrap_or_default()
            );
            format!("Basic {}", BASE64.encode(value))
        }
        None => String::new(),
    }
}

fn add_header(head: &[u8], name: &str, value: &str) -> Vec<u8> {
    if value.is_empty() {
        return head.to_vec();
    }
    let mut output = head[..head.len().saturating_sub(4)].to_vec();
    output.extend_from_slice(format!("\r\n{name}: {value}\r\n\r\n").as_bytes());
    output
}

fn strip_proxy_authorization(head: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(head.len());
    let mut skipping = false;
    for (index, line) in head.split_inclusive(|byte| *byte == b'\n').enumerate() {
        if index == 0 {
            output.extend_from_slice(line);
            continue;
        }
        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
            skipping = line
                .split(|byte| *byte == b':')
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case(b"proxy-authorization"));
        }
        if !skipping {
            output.extend_from_slice(line);
        }
    }
    output
}

fn rewrite_request_line(head: &[u8], replacement: &str) -> Vec<u8> {
    let end = head
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(head.len());
    let mut output = replacement.as_bytes().to_vec();
    output.extend_from_slice(&head[end..]);
    output
}

async fn http_connect(proxy: &Proxy, host: &str, port: u16) -> Result<(TcpStream, Vec<u8>)> {
    let mut stream = connect_tcp(&proxy.host, proxy.port).await?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if proxy.username.is_some() {
        request.push_str(&format!(
            "Proxy-Authorization: {}\r\n",
            proxy_authorization(proxy)
        ));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await?;
    let (head, pending) =
        tokio::time::timeout(Duration::from_secs(15), read_http_head(&mut stream)).await??;
    let status =
        String::from_utf8_lossy(head.split(|byte| *byte == b'\n').next().unwrap_or_default());
    if !(status.contains(" 200 ") || status.ends_with(" 200\r")) {
        bail!("upstream HTTP proxy CONNECT failed: {}", status.trim());
    }
    Ok((stream, pending))
}

async fn socks_connect(proxy: &Proxy, host: &str, port: u16) -> Result<(TcpStream, Vec<u8>)> {
    let mut stream = connect_tcp(&proxy.host, proxy.port).await?;
    if let Some(username) = &proxy.username {
        stream.write_all(&[5, 2, 0, 2]).await?;
        let mut method = [0u8; 2];
        stream.read_exact(&mut method).await?;
        if method != [5, 2] {
            bail!("SOCKS5 authentication method rejected");
        }
        let username = username.as_bytes();
        let password = proxy.password.as_deref().unwrap_or_default().as_bytes();
        if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
            bail!("SOCKS5 credentials exceed protocol limits");
        }
        let mut auth = vec![1, username.len() as u8];
        auth.extend_from_slice(username);
        auth.push(password.len() as u8);
        auth.extend_from_slice(password);
        stream.write_all(&auth).await?;
        let mut result = [0u8; 2];
        stream.read_exact(&mut result).await?;
        if result != [1, 0] {
            bail!("SOCKS5 credentials rejected");
        }
    } else {
        stream.write_all(&[5, 1, 0]).await?;
        let mut method = [0u8; 2];
        stream.read_exact(&mut method).await?;
        if method != [5, 0] {
            bail!("SOCKS5 no-auth method rejected");
        }
    }

    let mut request = vec![5, 1, 0];
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(ip) => {
                request.push(1);
                request.extend_from_slice(&ip.octets());
            }
            std::net::IpAddr::V6(ip) => {
                request.push(4);
                request.extend_from_slice(&ip.octets());
            }
        }
    } else {
        if host.len() > u8::MAX as usize {
            bail!("SOCKS5 host name exceeds protocol limits");
        }
        request.push(3);
        request.push(host.len() as u8);
        request.extend_from_slice(host.as_bytes());
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut response = [0u8; 4];
    stream.read_exact(&mut response).await?;
    if response[0] != 5 || response[1] != 0 {
        bail!("SOCKS5 connect failed (code {})", response[1]);
    }
    match response[3] {
        1 => {
            let mut rest = [0u8; 6];
            stream.read_exact(&mut rest).await?;
        }
        4 => {
            let mut rest = [0u8; 18];
            stream.read_exact(&mut rest).await?;
        }
        3 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut rest = vec![0; len[0] as usize + 2];
            stream.read_exact(&mut rest).await?;
        }
        other => bail!("invalid SOCKS5 address type {other}"),
    }
    Ok((stream, Vec::new()))
}

fn is_rate_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.contains("spawning process")
        || lower.contains("args=")
        || lower.contains("message=\"event\"")
        || !lower.contains("timestamp=")
    {
        return false;
    }
    [
        "quotaexceeded",
        "free usage limit",
        "rate limit",
        "rate-limit",
        "http 429",
        " 429",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

async fn run_watcher(paths: Paths) -> Result<()> {
    let _pid = match PidGuard::create(&paths, "watch", true) {
        Ok(guard) => guard,
        Err(error) if error.to_string().contains("already running") => {
            println!("watcher is already running; exiting");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    ensure_service(&paths, "daemon").await?;
    println!("proxy-rotate watcher — log: {}", paths.log_file.display());
    let mut offset: Option<u64> = None;
    let mut last_notice = Instant::now()
        .checked_sub(Duration::from_secs(30))
        .unwrap_or_else(Instant::now);
    let mut last_health = Instant::now();
    loop {
        if paths.log_file.exists() {
            let size = fs::metadata(&paths.log_file).map(|m| m.len()).unwrap_or(0);
            let start = offset.filter(|position| size >= *position).unwrap_or(size);
            let mut file = fs::File::open(&paths.log_file)?;
            file.seek(SeekFrom::Start(start))?;
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            while reader.read_line(&mut line)? > 0 {
                if is_rate_line(&line) && last_notice.elapsed() >= Duration::from_secs(20) {
                    last_notice = Instant::now();
                    println!("[limit] HTTP 429 detected; no proxy switch or retry. Honor Retry-After and provider terms.");
                }
                line.clear();
            }
            offset = Some(reader.stream_position()?);
        } else {
            offset = None;
        }
        if !port_open() {
            ensure_service(&paths, "daemon").await?;
        }
        if last_health.elapsed() >= Duration::from_secs(90) {
            last_health = Instant::now();
            health_check(&paths).await?;
        }
        tokio::select! {
            _ = sleep(Duration::from_millis(500)) => {},
            signal = tokio::signal::ctrl_c() => { signal?; break; }
        }
    }
    Ok(())
}

async fn health_check(paths: &Paths) -> Result<()> {
    let conn = match open_database(paths, false) {
        Ok(conn) => conn,
        Err(_) => return Ok(()),
    };
    if get_setting(&conn, "mode", "direct")? != "local" {
        return Ok(());
    }
    let Some(id) = current_proxy_id(paths, &conn)? else {
        return Ok(());
    };
    let proxy = get_proxy(&conn, id)?;
    let probe = probe_proxy(&proxy, Duration::from_secs(6)).await;
    if record_probe(&conn, &proxy, probe, "health check", false)? {
        return Ok(());
    }
    println!("[auto] proxy #{id} is unreachable; looking for a connectivity fallback (not a quota response)");
    auto_rotate_after_connectivity_failure(paths, &conn)
}

fn auto_rotate_after_connectivity_failure(paths: &Paths, conn: &Connection) -> Result<()> {
    // Called only after a failed connectivity probe; record_probe has already
    // updated the failure count/status. Do not turn a dead proxy back into a
    // cooldown entry here, or apply the cooldown twice.
    let current = current_proxy_id(paths, conn)?;
    if let Some(id) = current {
        log_event(
            conn,
            Some(id),
            "connectivity_failure",
            Some("proxy unreachable"),
        )?;
    }
    let Some(next) = pick_proxy(conn, current)? else {
        println!("[auto] no alternative proxy available; connection unchanged.");
        return Ok(());
    };
    if get_setting(conn, "mode", "direct")? == "local" {
        set_setting(conn, "current_id", next.id)?;
        conn.execute(
            "UPDATE proxies SET last_used=?1 WHERE id=?2",
            params![now(), next.id],
        )?;
        log_event(
            conn,
            Some(next.id),
            "rotate",
            Some("connectivity failure (local switch)"),
        )?;
        println!(
            "[auto] → #{} {} (local proxy switch)",
            next.id,
            next.display_url()
        );
    } else {
        apply_direct(paths, conn, &next)?;
        println!(
            "[auto] → #{} {} (restart OpenCode)",
            next.id,
            next.display_url()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_paths(root: &TempDir) -> Paths {
        Paths {
            state_dir: root.path().to_path_buf(),
            db: root.path().join("proxies.db"),
            env_file: root.path().join("proxy.env"),
            profile_file: root.path().join("profile.sh"),
            run_dir: root.path().join("run"),
            log_file: root.path().join("opencode.log"),
        }
    }

    #[test]
    fn client_proxy_credentials_are_not_forwarded_to_the_destination() {
        let request = b"GET http://example.test/ HTTP/1.1\r\nHost: example.test\r\nProxy-Authorization: Basic secret\r\n continuation\r\nAuthorization: Bearer upstream\r\n\r\n";
        assert_eq!(strip_proxy_authorization(request), b"GET http://example.test/ HTTP/1.1\r\nHost: example.test\r\nAuthorization: Bearer upstream\r\n\r\n");
    }

    #[test]
    fn duplicate_anonymous_proxies_are_rejected_without_modifying_the_pool() {
        let root = TempDir::new().unwrap();
        let conn = init_database(&test_paths(&root)).unwrap();
        let input = parse_proxy("http://proxy.example:8080").unwrap();
        insert_proxy(&conn, &input, None, None, None, None, false).unwrap();
        assert!(insert_proxy(&conn, &input, None, None, None, None, false).is_err());
        assert_eq!(list_proxies(&conn).unwrap().len(), 1);
    }

    #[test]
    fn expired_cooldowns_become_eligible_without_a_reset() {
        let root = TempDir::new().unwrap();
        let conn = init_database(&test_paths(&root)).unwrap();
        let input = parse_proxy("http://proxy.example:8080").unwrap();
        let id = insert_proxy(&conn, &input, None, None, None, None, false).unwrap();
        conn.execute(
            "UPDATE proxies SET status='cooldown',cooldown_until=?1 WHERE id=?2",
            params![timestamp_after(600), id],
        )
        .unwrap();
        assert!(pick_proxy(&conn, None).unwrap().is_none());
        conn.execute(
            "UPDATE proxies SET cooldown_until=?1 WHERE id=?2",
            params![timestamp_before(1), id],
        )
        .unwrap();
        assert_eq!(pick_proxy(&conn, None).unwrap().unwrap().id, id);
    }

    #[test]
    fn local_mode_does_not_silently_fall_back_to_a_direct_connection() {
        let root = TempDir::new().unwrap();
        let paths = test_paths(&root);
        let conn = init_database(&paths).unwrap();
        assert!(current_upstream(&paths).unwrap().is_none());
        set_setting(&conn, "mode", "local").unwrap();
        assert!(current_upstream(&paths).is_err());
        set_setting(&conn, "current_id", 123).unwrap();
        assert!(current_upstream(&paths).is_err());
    }

    #[test]
    fn connectivity_fallback_does_not_revive_a_dead_proxy() {
        let root = TempDir::new().unwrap();
        let paths = test_paths(&root);
        let conn = init_database(&paths).unwrap();
        let input = parse_proxy("http://proxy.example:8080").unwrap();
        let id = insert_proxy(&conn, &input, None, None, None, None, false).unwrap();
        set_setting(&conn, "mode", "local").unwrap();
        set_setting(&conn, "current_id", id).unwrap();
        conn.execute(
            "UPDATE proxies SET status='dead',fail_count=3 WHERE id=?1",
            [id],
        )
        .unwrap();
        auto_rotate_after_connectivity_failure(&paths, &conn).unwrap();
        assert_eq!(get_proxy(&conn, id).unwrap().status, "dead");
    }

    #[test]
    fn malformed_proxy_urls_do_not_disclose_passwords() {
        let error = parse_proxy("http://alice:topsecret@host:invalid").unwrap_err();
        assert!(!format!("{error:#}").contains("topsecret"));
        assert!(parse_proxy("http://host:8080/path").is_err());
    }

    #[test]
    fn quota_reasons_are_never_connectivity_rotation_reasons() {
        for reason in [
            "HTTP 429",
            "quota exceeded",
            "Free usage limit",
            "rate-limit",
        ] {
            assert!(is_rate_limit_reason(reason), "{reason}");
        }
        assert!(!is_rate_limit_reason("proxy unreachable"));
    }

    #[test]
    fn parses_supported_proxy_urls_and_keeps_credentials_out_of_display() {
        let parsed = parse_proxy("socks5://alice:secret@[::1]:1081").unwrap();
        assert_eq!(parsed.scheme, "socks5");
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.port, 1081);
        assert_eq!(parsed.username.as_deref(), Some("alice"));
        assert_eq!(parsed.password.as_deref(), Some("secret"));
        let proxy = Proxy {
            id: 1,
            label: None,
            scheme: parsed.scheme,
            host: parsed.host,
            port: parsed.port,
            username: parsed.username,
            password: parsed.password,
            country: None,
            status: "active".into(),
            fail_count: 0,
            ok_count: 0,
            last_used: None,
            last_ok: None,
            cooldown_until: None,
            note: None,
            source: None,
            last_seen: None,
            created_at: now(),
        };
        assert!(!proxy.display_url().contains("secret"));
        assert!(proxy.url().contains("secret"));
    }

    #[test]
    fn http_429_probe_is_recorded_as_reachable_and_does_not_switch() {
        let temp = TempDir::new().unwrap();
        let paths = test_paths(&temp);
        let conn = init_database(&paths).unwrap();
        let input = parse_proxy("http://proxy.example:8080").unwrap();
        let id = insert_proxy(&conn, &input, None, None, None, None, false).unwrap();
        let proxy = get_proxy(&conn, id).unwrap();
        assert!(record_probe(
            &conn,
            &proxy,
            ProbeResult {
                status: Some(429),
                elapsed: Duration::from_millis(3)
            },
            "test",
            false,
        )
        .unwrap());
        let updated = get_proxy(&conn, id).unwrap();
        assert_eq!(updated.status, "active");
        assert_eq!(updated.fail_count, 0);
        assert_eq!(updated.ok_count, 1);
        assert_eq!(get_setting(&conn, "mode", "direct").unwrap(), "direct");
        assert_eq!(current_proxy_id(&paths, &conn).unwrap(), None);
    }
}
