//! Optional Redis/Valkey L2. Local hits stay local and network failures are misses.
use anyhow::{Result, ensure};
use std::{
    sync::{Arc, OnceLock, RwLock, atomic::{AtomicU64, Ordering}, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub url: String,
    pub namespace: String,
    pub timeout_ms: u64,
    pub ttl_seconds: u64,
    pub max_value_bytes: usize,
    pub max_fills_per_second: usize,
    pub tiles: bool,
    pub indexes: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { url: String::new(), namespace: "river".into(), timeout_ms: 8,
            ttl_seconds: 3600, max_value_bytes: 2 * 1024 * 1024, max_fills_per_second: 128,
            tiles: true, indexes: true }
    }
}

impl Settings {
    pub fn environment(configured: Option<Self>) -> Result<Option<Self>> {
        let mut settings = configured.unwrap_or_default();
        macro_rules! read {
            ($key:literal, $field:ident) => {
                if let Ok(value) = std::env::var($key) {
                    settings.$field = value.parse().map_err(|_| anyhow::anyhow!("Invalid value for {}", $key))?;
                }
            };
        }
        read!("RIVER_SHARED_CACHE_URL", url);
        read!("RIVER_SHARED_CACHE_NAMESPACE", namespace);
        read!("RIVER_SHARED_CACHE_TIMEOUT_MS", timeout_ms);
        read!("RIVER_SHARED_CACHE_TTL_SECONDS", ttl_seconds);
        read!("RIVER_SHARED_CACHE_MAX_VALUE_BYTES", max_value_bytes);
        read!("RIVER_SHARED_CACHE_MAX_FILLS_PER_SECOND", max_fills_per_second);
        read!("RIVER_SHARED_CACHE_TILES", tiles);
        read!("RIVER_SHARED_CACHE_INDEXES", indexes);
        if settings.url.is_empty() { return Ok(None); }
        settings.client()?;
        Ok(Some(settings))
    }

    fn client(&self) -> Result<redis::Client> {
        ensure!((1..=250).contains(&self.timeout_ms), "Shared cache timeout must be 1 through 250 ms");
        ensure!((1..=604800).contains(&self.ttl_seconds), "Shared cache TTL must be 1 through 604800 seconds");
        ensure!((1024..=8 * 1024 * 1024).contains(&self.max_value_bytes), "Shared cache value limit must be 1 KiB through 8 MiB");
        ensure!((1..=4096).contains(&self.max_fills_per_second), "Shared cache fill rate must be 1 through 4096 per second");
        ensure!(!self.namespace.is_empty() && self.namespace.len() <= 80 &&
            self.namespace.bytes().all(|byte| byte.is_ascii_alphanumeric() || b":-_."
                .contains(&byte)), "Shared cache namespace must contain 1 through 80 safe ASCII characters");
        let (scheme, endpoint) = self.url.split_once("://")
            .ok_or_else(|| anyhow::anyhow!("Invalid shared cache endpoint"))?;
        ensure!(matches!(scheme, "redis" | "rediss" | "valkey" | "valkeys") &&
            !endpoint.is_empty() && !endpoint.contains('#'), "Use a Redis or Valkey TCP endpoint");
        // RESP is shared. Normalize the Valkey scheme for clients that accept only Redis URLs.
        let endpoint = self.url.replacen("valkey", "redis", usize::from(scheme.starts_with("valkey")));
        redis::Client::open(endpoint).map_err(|_| anyhow::anyhow!("Invalid shared cache endpoint"))
    }
}

#[derive(Default)]
struct Counters {
    hits: AtomicU64,
    tile_hits: AtomicU64,
    batched_tile_lookups: AtomicU64,
    index_hits: AtomicU64,
    misses: AtomicU64,
    failures: AtomicU64,
    fills: AtomicU64,
    skipped_fills: AtomicU64,
    received_bytes: AtomicU64,
    sent_bytes: AtomicU64,
}

enum Read {
    Get(String, mpsc::SyncSender<Option<Vec<u8>>>),
    GetBatch(Vec<String>, mpsc::SyncSender<Option<Vec<Vec<u8>>>>),
    Probe(mpsc::SyncSender<bool>),
}

struct Backend {
    settings: Settings,
    reads: mpsc::SyncSender<Read>,
    writes: mpsc::SyncSender<(String, Vec<u8>)>,
    counters: Arc<Counters>,
    server: Arc<RwLock<Option<serde_json::Value>>>,
}

fn active() -> &'static RwLock<Option<Arc<Backend>>> {
    static ACTIVE: OnceLock<RwLock<Option<Arc<Backend>>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

fn selected() -> Option<Arc<Backend>> {
    active().read().unwrap_or_else(|error| error.into_inner()).clone()
}

static STARTUP: OnceLock<Option<Settings>> = OnceLock::new();
static TILES_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn install(backend: Option<Arc<Backend>>) {
    let mut active = active().write().unwrap_or_else(|error| error.into_inner());
    let tiles = backend.as_ref().is_some_and(|backend| backend.settings.tiles);
    *active = backend;
    TILES_ENABLED.store(tiles, Ordering::Release);
}

pub fn tiles_enabled() -> bool { TILES_ENABLED.load(Ordering::Acquire) }

struct Connection {
    client: redis::Client,
    current: Option<redis::Connection>,
    timeout: Duration,
    retry_after: Instant,
}

impl Connection {
    fn new(client: redis::Client, timeout: Duration) -> Self {
        Self { client, current: None, timeout, retry_after: Instant::now() }
    }
    fn command<T: redis::FromRedisValue>(&mut self, command: &redis::Cmd) -> Option<T> {
        self.run(|connection| command.query(connection))
    }
    fn run<T>(&mut self, operation: impl FnOnce(&mut redis::Connection) -> redis::RedisResult<T>) -> Option<T> {
        if Instant::now() < self.retry_after { return None; }
        let result = (|| -> redis::RedisResult<T> {
            if self.current.is_none() {
                let connection = self.client.get_connection_with_timeout(self.timeout)?;
                connection.set_read_timeout(Some(self.timeout))?;
                connection.set_write_timeout(Some(self.timeout))?;
                self.current = Some(connection);
            }
            operation(self.current.as_mut().unwrap())
        })();
        match result {
            Ok(value) => Some(value),
            Err(_) => {
                self.current = None;
                self.retry_after = Instant::now() + Duration::from_millis(250);
                None
            }
        }
    }
}

impl Backend {
    fn new(settings: Settings, verify: bool) -> Result<Arc<Self>> {
        let client = settings.client()?;
        let timeout = Duration::from_millis(settings.timeout_ms);
        let ttl = settings.ttl_seconds;
        let limit = settings.max_value_bytes;
        let max_fills = settings.max_fills_per_second;
        let (reads, read_queue) = mpsc::sync_channel(16);
        let (writes, write_queue) = mpsc::sync_channel::<(String, Vec<u8>)>((16 * 1024 * 1024 / limit).min(32));
        let counters = Arc::new(Counters::default());
        let server = Arc::new(RwLock::new(None));
        let mut read = Connection::new(client.clone(), timeout);
        std::thread::Builder::new().name("river-cache-reads".into()).spawn(move || {
            while let Ok(job) = read_queue.recv() {
                match job {
                    Read::Get(key, response) => {
                        let result = read.command::<Vec<u8>>(redis::cmd("GETRANGE").arg(key).arg(0).arg(limit));
                        let _ = response.try_send(result);
                    }
                    Read::GetBatch(keys, response) => {
                        let mut pipeline = redis::pipe();
                        for key in keys {
                            pipeline.cmd("GETRANGE").arg(key).arg(0).arg(limit);
                        }
                        let result = read.run(|connection| pipeline.query(connection));
                        let _ = response.try_send(result);
                    }
                    Read::Probe(response) => {
                        let _ = response.try_send(read.command::<String>(&redis::cmd("PING")).is_some());
                    }
                }
            }
        })?;
        let backend = Arc::new(Self { settings, reads, writes, counters: counters.clone(), server: server.clone() });
        if verify {
            let (send, receive) = mpsc::sync_channel(1);
            ensure!(backend.reads.try_send(Read::Probe(send)).is_ok() &&
                receive.recv_timeout(Duration::from_secs(1)) == Ok(true),
                "Shared cache connection or authentication failed");
        }
        let mut telemetry = Connection::new(client.clone(), timeout);
        let mut write = Connection::new(client, timeout);
        std::thread::Builder::new().name("river-cache-fills".into()).spawn(move || {
            let mut next_info = Instant::now();
            let mut window = Instant::now();
            let mut attempts = 0;
            loop {
                if Instant::now() >= next_info {
                    let status = server_info(&mut telemetry);
                    *server.write().unwrap_or_else(|error| error.into_inner()) = status;
                    next_info = Instant::now() + Duration::from_secs(30);
                }
                match write_queue.recv_timeout(Duration::from_secs(1)) {
                    Ok((key, value)) => {
                        if window.elapsed() >= Duration::from_secs(1) {
                            window = Instant::now();
                            attempts = 0;
                        }
                        if attempts >= max_fills {
                            counters.skipped_fills.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        attempts += 1;
                        if write.command::<Option<String>>(redis::cmd("SET").arg(key).arg(&value)
                            .arg("EX").arg(ttl).arg("NX")).is_some_and(|v| v.is_some()) {
                            counters.fills.fetch_add(1, Ordering::Relaxed);
                            counters.sent_bytes.fetch_add(value.len() as u64, Ordering::Relaxed);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })?;
        Ok(backend)
    }

    fn key(&self, kind: &str, key: &str) -> Option<String> {
        let enabled = match kind { "tile" => self.settings.tiles, "index" => self.settings.indexes, _ => false };
        enabled.then(|| format!("{}:v1:{kind}:{key}", self.settings.namespace))
    }
}

fn server_info(connection: &mut Connection) -> Option<serde_json::Value> {
    let info = connection.command::<String>(&redis::cmd("INFO"))?;
    let mut stats = serde_json::Map::new();
    for line in info.lines() {
        let Some((name, value)) = line.split_once(':') else { continue; };
        if matches!(name, "used_memory" | "maxmemory" | "evicted_keys" | "keyspace_hits" | "keyspace_misses") {
            if let Ok(value) = value.parse::<u64>() { stats.insert(name.to_owned(), value.into()); }
        }
        if matches!(name, "redis_version" | "valkey_version" | "maxmemory_policy") {
            stats.insert(name.to_owned(), value.into());
        }
        if name.starts_with("db") && name[2..].chars().all(|character| character.is_ascii_digit()) {
            if let Some(keys) = value.split(',').next().and_then(|value| value.strip_prefix("keys="))
                .and_then(|value| value.parse::<u64>().ok()) {
                stats.insert(format!("{name}_keys"), keys.into());
            }
        }
    }
    Some(stats.into())
}

pub fn initialize(configured: Option<Settings>) -> Result<()> {
    let settings = Settings::environment(configured)?;
    let backend = settings.clone().map(|settings| Backend::new(settings, false)).transpose()?;
    let _ = STARTUP.set(settings);
    install(backend);
    Ok(())
}

pub async fn replace(settings: Option<Settings>) -> Result<()> {
    let backend = tokio::task::spawn_blocking(move || settings.map(|settings| Backend::new(settings, true)).transpose()).await??;
    install(backend);
    Ok(())
}

pub async fn reset() -> Result<()> { replace(STARTUP.get().cloned().flatten()).await }

pub fn get(kind: &str, key: &str) -> Option<Vec<u8>> {
    let backend = selected()?;
    let key = backend.key(kind, key)?;
    let (send, receive) = mpsc::sync_channel(1);
    let result = backend.reads.try_send(Read::Get(key, send)).ok()
        .and_then(|_| receive.recv_timeout(Duration::from_millis(backend.settings.timeout_ms)).ok())
        .flatten();
    record_read(&backend, kind, result)
}

fn record_read(backend: &Backend, kind: &str, result: Option<Vec<u8>>) -> Option<Vec<u8>> {
    match result {
        Some(value) if !value.is_empty() && value.len() <= backend.settings.max_value_bytes => {
            backend.counters.hits.fetch_add(1, Ordering::Relaxed);
            match kind {
                "tile" => &backend.counters.tile_hits,
                _ => &backend.counters.index_hits,
            }.fetch_add(1, Ordering::Relaxed);
            backend.counters.received_bytes.fetch_add(value.len() as u64, Ordering::Relaxed);
            Some(value)
        }
        Some(_) => { backend.counters.misses.fetch_add(1, Ordering::Relaxed); None }
        None => { backend.counters.failures.fetch_add(1, Ordering::Relaxed); None }
    }
}

/// One bounded network round trip for a small group of encoded tile keys.
/// Return results in key order. Cap the aggregate response even when every
/// stored value is at the configured maximum.
pub fn get_tiles(keys: &[String]) -> Vec<Option<Vec<u8>>> {
    let Some(backend) = selected().filter(|backend| backend.settings.tiles) else { return vec![]; };
    let max_keys = (8 * 1024 * 1024 / (backend.settings.max_value_bytes + 1)).clamp(1, 16);
    let keys = keys.iter().take(max_keys).filter_map(|key| backend.key("tile", key)).collect::<Vec<_>>();
    if keys.is_empty() { return vec![]; }
    let expected = keys.len();
    let (send, receive) = mpsc::sync_channel(1);
    backend.counters.batched_tile_lookups.fetch_add(1, Ordering::Relaxed);
    let values = backend.reads.try_send(Read::GetBatch(keys, send)).ok()
        .and_then(|_| receive.recv_timeout(Duration::from_millis(backend.settings.timeout_ms)).ok())
        .flatten().filter(|values| values.len() == expected);
    match values {
        Some(values) => values.into_iter().map(|value| record_read(&backend, "tile", Some(value))).collect(),
        None => { backend.counters.failures.fetch_add(1, Ordering::Relaxed); vec![] }
    }
}

pub fn admit(kind: &str, bytes: usize) -> bool {
    selected().is_some_and(|backend| bytes <= backend.settings.max_value_bytes && backend.key(kind, "").is_some())
}

pub fn fill(kind: &str, key: &str, value: Vec<u8>) {
    let Some(backend) = selected() else { return; };
    if value.len() > backend.settings.max_value_bytes { return; }
    let Some(key) = backend.key(kind, key) else { return; };
    if backend.writes.try_send((key, value)).is_err() {
        backend.counters.skipped_fills.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn diagnostics() -> serde_json::Value {
    let Some(backend) = selected() else { return serde_json::json!({"configured":false}); };
    let server = backend.server.read().unwrap_or_else(|error| error.into_inner());
    let c = &backend.counters;
    serde_json::json!({
        "configured":true, "namespace":backend.settings.namespace, "tiles":backend.settings.tiles,
        "indexes":backend.settings.indexes, "timeoutMs":backend.settings.timeout_ms,
        "ttlSeconds":backend.settings.ttl_seconds, "maxValueBytes":backend.settings.max_value_bytes,
        "maxFillsPerSecond":backend.settings.max_fills_per_second,
        "server":*server, "serverMetricsScope":"entire server; exact namespace size is unknown",
        "hits":c.hits.load(Ordering::Relaxed), "misses":c.misses.load(Ordering::Relaxed),
        "tileHits":c.tile_hits.load(Ordering::Relaxed), "indexHits":c.index_hits.load(Ordering::Relaxed),
        "batchedTileLookups":c.batched_tile_lookups.load(Ordering::Relaxed),
        "failures":c.failures.load(Ordering::Relaxed), "fills":c.fills.load(Ordering::Relaxed),
        "droppedFills":c.skipped_fills.load(Ordering::Relaxed),
        "receivedBytes":c.received_bytes.load(Ordering::Relaxed),
        "sentBytes":c.sent_bytes.load(Ordering::Relaxed)
    })
}
