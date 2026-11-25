//! Optional Redis/Valkey L2. Local hits stay local and network failures are misses.
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, OnceLock, RwLock, atomic::{AtomicU64, Ordering}, mpsc},
    time::{Duration, Instant},
};

/// Stable IDs keep routing unchanged when credentials rotate or list order changes.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Endpoint {
    pub id: String,
    pub url: String,
    #[serde(default = "one")]
    pub weight: u32,
}
fn one() -> u32 { 1 }

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub url: String,
    pub endpoints: Vec<Endpoint>,
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
        Self { url: String::new(), endpoints: Vec::new(), namespace: "river".into(), timeout_ms: 8,
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
        if let Ok(value) = std::env::var("RIVER_SHARED_CACHE_URL") {
            settings.url = value;
            settings.endpoints.clear();
        }
        if let Ok(value) = std::env::var("RIVER_SHARED_CACHE_ENDPOINTS") {
            settings.endpoints = serde_json::from_str(&value)
                .map_err(|_| anyhow::anyhow!("RIVER_SHARED_CACHE_ENDPOINTS must be a JSON array of id, url and optional weight"))?;
            settings.url.clear();
        }
        read!("RIVER_SHARED_CACHE_NAMESPACE", namespace);
        read!("RIVER_SHARED_CACHE_TIMEOUT_MS", timeout_ms);
        read!("RIVER_SHARED_CACHE_TTL_SECONDS", ttl_seconds);
        read!("RIVER_SHARED_CACHE_MAX_VALUE_BYTES", max_value_bytes);
        read!("RIVER_SHARED_CACHE_MAX_FILLS_PER_SECOND", max_fills_per_second);
        read!("RIVER_SHARED_CACHE_TILES", tiles);
        read!("RIVER_SHARED_CACHE_INDEXES", indexes);
        if settings.url.is_empty() && settings.endpoints.is_empty() { return Ok(None); }
        settings.validate_endpoints()?;
        Ok(Some(settings))
    }

    fn validate_endpoints(&self) -> Result<Vec<Endpoint>> {
        ensure!(self.url.is_empty() || self.endpoints.is_empty(), "Choose sharedCache.url or sharedCache.endpoints, not both");
        let endpoints = if self.endpoints.is_empty() {
            vec![Endpoint { id: "default".into(), url: self.url.clone(), weight: 1 }]
        } else { self.endpoints.clone() };
        let mut ids = std::collections::HashSet::new();
        for endpoint in &endpoints {
            ensure!(!endpoint.id.is_empty() && endpoint.id.len() <= 80 && endpoint.id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)), "Shared cache endpoint IDs must contain 1 through 80 letters, digits, hyphens or underscores");
            ensure!(ids.insert(&endpoint.id), "Shared cache endpoint IDs must be unique");
            ensure!((1..=1000).contains(&endpoint.weight), "Shared cache endpoint weight must be 1 through 1000");
            let mut single = self.clone(); single.url = endpoint.url.clone(); single.client()?;
        }
        Ok(endpoints)
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
    writes: mpsc::SyncSender<Fill>,
    counters: Arc<Counters>,
    server: Arc<RwLock<Option<serde_json::Value>>>,
}

// All endpoints share one queue-memory ceiling and fill-rate window.
const MAX_PENDING_FILL_BYTES: u64 = 16 * 1024 * 1024;
struct FillBudget {
    pending: AtomicU64,
    window: std::sync::Mutex<(Instant, usize)>,
    rate: usize,
}
struct Fill { key: String, value: Vec<u8>, budget: Arc<FillBudget> }
impl Drop for Fill {
    fn drop(&mut self) { self.budget.pending.fetch_sub(self.value.len() as u64, Ordering::Relaxed); }
}
impl FillBudget {
    fn reserve(self: &Arc<Self>, key: String, value: Vec<u8>) -> Option<Fill> {
        let mut window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        if window.0.elapsed() >= Duration::from_secs(1) { *window = (Instant::now(), 0); }
        if window.1 >= self.rate { return None; }
        let bytes = value.len() as u64;
        self.pending.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |n| n.checked_add(bytes).filter(|total| *total <= MAX_PENDING_FILL_BYTES)).ok()?;
        window.1 += 1;
        Some(Fill { key, value, budget: self.clone() })
    }
}
const ROUTING_BUCKETS: usize = 4096;
struct Pool {
    settings: Settings,
    endpoints: Vec<Endpoint>,
    backends: Vec<Arc<Backend>>,
    routing: Vec<usize>,
    fills: Arc<FillBudget>,
}
fn routing(endpoints: &[Endpoint]) -> Vec<usize> {
    (0..ROUTING_BUCKETS).map(|bucket| {
        endpoints.iter().enumerate().map(|(index, endpoint)| {
            let mut hash = Sha256::new(); hash.update((bucket as u32).to_be_bytes()); hash.update(endpoint.id.as_bytes());
            let value = u64::from_be_bytes(hash.finalize()[..8].try_into().unwrap());
            let u = ((value >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0);
            (index, -u.ln() / f64::from(endpoint.weight))
        }).min_by(|a,b| a.1.total_cmp(&b.1)).unwrap().0
    }).collect()
}
impl Pool {
    fn new(settings: Settings, verify: bool) -> Result<Arc<Self>> {
        let endpoints = settings.validate_endpoints()?;
        let routing = routing(&endpoints);
        let mut backends = Vec::new();
        for endpoint in &endpoints {
            let mut single = settings.clone(); single.url = endpoint.url.clone(); single.endpoints.clear();
            backends.push(Backend::new(single, verify)?);
        }
        let fills = Arc::new(FillBudget { pending: AtomicU64::new(0), window: std::sync::Mutex::new((Instant::now(), 0)), rate: settings.max_fills_per_second });
        Ok(Arc::new(Self { settings, endpoints, backends, routing, fills }))
    }
    fn index(&self, key: &str) -> usize {
        let digest = Sha256::digest(key.as_bytes());
        self.routing[u32::from_be_bytes(digest[..4].try_into().unwrap()) as usize % ROUTING_BUCKETS]
    }
    fn backend(&self, key: &str) -> &Arc<Backend> { &self.backends[self.index(key)] }
}

fn active() -> &'static RwLock<Option<Arc<Pool>>> {
    static ACTIVE: OnceLock<RwLock<Option<Arc<Pool>>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

fn selected() -> Option<Arc<Pool>> {
    active().read().unwrap_or_else(|error| error.into_inner()).clone()
}

static STARTUP: OnceLock<Option<Settings>> = OnceLock::new();
static TILES_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn install(backend: Option<Arc<Pool>>) {
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
        let (reads, read_queue) = mpsc::sync_channel(16);
        let (writes, write_queue) = mpsc::sync_channel::<Fill>((16 * 1024 * 1024 / limit).min(32));
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
            loop {
                if Instant::now() >= next_info {
                    let status = server_info(&mut telemetry);
                    *server.write().unwrap_or_else(|error| error.into_inner()) = status;
                    next_info = Instant::now() + Duration::from_secs(30);
                }
                match write_queue.recv_timeout(Duration::from_secs(1)) {
                    Ok(fill) => {
                        let key = &fill.key; let value = &fill.value;
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
    let backend = settings.clone().map(|settings| Pool::new(settings, false)).transpose()?;
    let _ = STARTUP.set(settings);
    install(backend);
    Ok(())
}

pub async fn replace(settings: Option<Settings>) -> Result<()> {
    let backend = tokio::task::spawn_blocking(move || settings.map(|settings| Pool::new(settings, true)).transpose()).await??;
    install(backend);
    Ok(())
}

pub async fn reset() -> Result<()> { replace(STARTUP.get().cloned().flatten()).await }

pub fn get(kind: &str, key: &str) -> Option<Vec<u8>> {
    let pool = selected()?;
    let backend = pool.backend(key);
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
    let Some(pool) = selected().filter(|pool| pool.settings.tiles) else { return vec![]; };
    let count = keys.len().min((8 * 1024 * 1024 / (pool.settings.max_value_bytes + 1)).clamp(1,16));
    let mut groups = std::collections::BTreeMap::<usize, Vec<(usize, String)>>::new();
    for (position,key) in keys.iter().take(count).enumerate() {
        let index = pool.index(key);
        if let Some(key) = pool.backends[index].key("tile",key) { groups.entry(index).or_default().push((position,key)); }
    }
    let deadline = Instant::now() + Duration::from_millis(pool.settings.timeout_ms);
    let mut pending = Vec::new();
    for (index, group) in groups {
        let backend = &pool.backends[index];
        let (send, receive) = mpsc::sync_channel(1);
        backend.counters.batched_tile_lookups.fetch_add(1, Ordering::Relaxed);
        if backend.reads.try_send(Read::GetBatch(group.iter().map(|(_,key)|key.clone()).collect(),send)).is_ok() {
            pending.push((index,group,receive));
        } else { backend.counters.failures.fetch_add(1,Ordering::Relaxed); }
    }
    let mut result = vec![None;count];
    for (index,group,receive) in pending {
        let backend = &pool.backends[index];
        let response = receive.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok().flatten();
        if let Some(values) = response.filter(|v|v.len()==group.len()) {
            for ((position,_),value) in group.into_iter().zip(values) { result[position]=record_read(backend,"tile",Some(value)); }
        } else { backend.counters.failures.fetch_add(1,Ordering::Relaxed); }
    }
    result
}

pub fn admit(kind: &str, bytes: usize) -> bool {
    selected().is_some_and(|backend| bytes <= backend.settings.max_value_bytes && backend.backends[0].key(kind, "").is_some())
}

pub fn fill(kind: &str, key: &str, value: Vec<u8>) {
    let Some(pool) = selected() else { return; };
    let backend = pool.backend(key);
    if value.len() > backend.settings.max_value_bytes { return; }
    let Some(key) = backend.key(kind, key) else { return; };
    let Some(fill) = pool.fills.reserve(key, value) else {
        backend.counters.skipped_fills.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if backend.writes.try_send(fill).is_err() {
        backend.counters.skipped_fills.fetch_add(1, Ordering::Relaxed);
    }
}

fn backend_diagnostics(backend: &Backend) -> serde_json::Value {
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

/// Endpoints never expose URLs or credentials in diagnostics.
pub fn diagnostics() -> serde_json::Value {
    let Some(pool)=selected() else {return serde_json::json!({"configured":false});};
    let mut result=backend_diagnostics(&pool.backends[0]);
    for field in ["hits","misses","tileHits","indexHits","batchedTileLookups","failures","fills","droppedFills","receivedBytes","sentBytes"] {
        result[field]=serde_json::json!(pool.backends.iter().map(|b|backend_diagnostics(b)[field].as_u64().unwrap_or(0)).sum::<u64>());
    }
    result["pendingFillBytes"]=serde_json::json!(pool.fills.pending.load(Ordering::Relaxed));
    result["maxPendingFillBytes"]=serde_json::json!(MAX_PENDING_FILL_BYTES);
    result["fillRateScope"]=serde_json::json!("all endpoints combined");
    result["endpoints"]=serde_json::json!(pool.endpoints.iter().zip(&pool.backends).map(|(e,b)|serde_json::json!({"id":e.id,"weight":e.weight,"metrics":backend_diagnostics(b)})).collect::<Vec<_>>());
    result["routing"]=serde_json::json!({"algorithm":"weighted rendezvous bucket table", "buckets":ROUTING_BUCKETS,"lookup":"one key hash and constant-time table lookup","replication":false,"batchTimeoutScope":"one deadline across all endpoints"});
    if pool.backends.len()>1 {result["server"]=serde_json::Value::Null;}
    result
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    fn endpoints(ids: &[&str]) -> Vec<Endpoint> {ids.iter().map(|id|Endpoint{id:(*id).into(),url:"redis://localhost".into(),weight:1}).collect()}
    #[test]
    fn order_and_credentials_do_not_move_keys() {
        let a=endpoints(&["a","b","c"]);let mut b=endpoints(&["c","a","b"]);b[0].url="rediss://rotated".into();
        for (i,j) in routing(&a).into_iter().zip(routing(&b)) {assert_eq!(a[i].id,b[j].id);}
    }
    #[test]
    fn adding_node_only_moves_its_share() {
        let a=endpoints(&["a","b"]);let b=endpoints(&["a","b","c"]);
        let mut moved=0;
        for (i,j) in routing(&a).into_iter().zip(routing(&b)) {if i!=j {assert_eq!(j,2);moved+=1;}}
        assert!((1000..1800).contains(&moved));
    }
    #[test]
    fn invalid_membership_is_rejected() {
        let mut s=Settings::default();s.endpoints=endpoints(&["a","a"]);assert!(s.validate_endpoints().is_err());
        s.endpoints=endpoints(&["a"]);s.endpoints[0].weight=0;assert!(s.validate_endpoints().is_err());
    }
    #[test]
    fn fill_budget_is_global_and_releases_dropped_work() {
        let budget=Arc::new(FillBudget{pending:AtomicU64::new(0),window:std::sync::Mutex::new((Instant::now(),0)),rate:2});
        let first=budget.reserve("one".into(),vec![0;8*1024*1024]).unwrap();
        let second=budget.reserve("two".into(),vec![0;8*1024*1024]).unwrap();
        assert_eq!(budget.pending.load(Ordering::Relaxed),MAX_PENDING_FILL_BYTES);
        assert!(budget.reserve("three".into(),vec![0]).is_none());
        drop(first);drop(second);assert_eq!(budget.pending.load(Ordering::Relaxed),0);
        assert!(budget.reserve("four".into(),vec![0]).is_none());
    }
    #[tokio::test]
    #[ignore = "requires controlled Redis and Valkey services on localhost ports 16379 and 16380"]
    async fn real_redis_valkey_restart_routing_and_failed_replacement() {
        let namespace=format!("river-test-{}",uuid::Uuid::new_v4());
        let mut settings=Settings{namespace,timeout_ms:100,max_fills_per_second:4096,..Settings::default()};
        settings.endpoints=vec![Endpoint{id:"redis".into(),url:"redis://127.0.0.1:16379".into(),weight:1},Endpoint{id:"valkey".into(),url:"valkey://127.0.0.1:16380".into(),weight:1}];
        replace(Some(settings.clone())).await.unwrap();
        let keys=(0..64).map(|i|format!("fixture-{i}")).collect::<Vec<_>>();
        for key in &keys {fill("tile",key,key.as_bytes().to_vec());}
        let deadline=Instant::now()+Duration::from_secs(5);
        loop {
            if keys.iter().all(|key|get("tile",key).as_deref()==Some(key.as_bytes())){break;}
            assert!(Instant::now()<deadline,"asynchronous cache fills did not complete");
            for key in &keys { if get("tile",key).is_none() {fill("tile",key,key.as_bytes().to_vec());} }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(diagnostics()["endpoints"].as_array().unwrap().iter().all(|e|e["metrics"]["fills"].as_u64().unwrap()>0));
        // Drop every client and recreate in reverse order: remote values remain reachable.
        replace(None).await.unwrap();settings.endpoints.reverse();replace(Some(settings.clone())).await.unwrap();
        for key in &keys {assert_eq!(get("tile",key).as_deref(),Some(key.as_bytes()));}
        let original=selected().unwrap();let mut invalid=settings;invalid.endpoints[0].url="redis://127.0.0.1:1".into();
        assert!(replace(Some(invalid)).await.is_err());assert!(Arc::ptr_eq(&original,&selected().unwrap()));
        assert_eq!(get("tile",&keys[0]).as_deref(),Some(keys[0].as_bytes()));
        replace(None).await.unwrap();
    }

}
