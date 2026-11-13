//! Demand-loaded remote handles are reclaimable metadata, not an unbounded catalog.
use crate::{AssetId, RegisteredAsset, resources};
use std::{
    collections::{BTreeMap, HashMap},
    ops::Deref,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};
static OWNED: AtomicUsize = AtomicUsize::new(0);
#[derive(Debug, Default)]
pub struct Cost {
    state: Mutex<(usize, usize)>,
}
impl Cost {
    pub fn resize(&self, bytes: usize) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.1 > 0 {
            if bytes > s.0 {
                OWNED.fetch_add(bytes - s.0, Ordering::AcqRel);
            } else {
                OWNED.fetch_sub(s.0 - bytes, Ordering::AcqRel);
            }
        }
        s.0 = bytes;
    }
    fn retain(self: &Arc<Self>) -> Hold {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.1 == 0 {
            OWNED.fetch_add(s.0, Ordering::AcqRel);
        }
        s.1 += 1;
        Hold(self.clone())
    }
}
struct Hold(Arc<Cost>);
impl Drop for Hold {
    fn drop(&mut self) {
        let mut s = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        s.1 -= 1;
        if s.1 == 0 {
            OWNED.fetch_sub(s.0, Ordering::AcqRel);
        }
    }
}
struct Entry {
    bytes: usize,
    tick: u64,
    created: Instant,
    _hold: Option<Hold>,
}
#[derive(Default)]
struct Tracking {
    entries: HashMap<AssetId, Entry>,
    lru: BTreeMap<u64, AssetId>,
    bytes: usize,
    clock: u64,
}
pub(crate) struct Assets {
    values: BTreeMap<AssetId, RegisteredAsset>,
    tracked: Mutex<Tracking>,
}
impl Deref for Assets {
    type Target = BTreeMap<AssetId, RegisteredAsset>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl Assets {
    pub fn shared(values: BTreeMap<AssetId, RegisteredAsset>) -> Arc<tokio::sync::RwLock<Self>> {
        let cache = Arc::new(tokio::sync::RwLock::new(Self {
            values,
            tracked: Mutex::new(Tracking::default()),
        }));
        let weak = Arc::downgrade(&cache);
        resources::register("asset_metadata", move |limit| {
            let Some(c) = weak.upgrade() else {
                return Default::default();
            };
            if let Ok(mut c) = c.try_write() {
                c.trim(limit)
            } else {
                Default::default()
            }
        });
        cache
    }
    pub fn get(&self, id: &AssetId) -> Option<&RegisteredAsset> {
        let found = self.values.get(id);
        if found.is_some() {
            let mut s = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
            s.clock += 1;
            let now = s.clock;
            if let Some(e) = s.entries.get_mut(id) {
                let old = e.tick;
                e.tick = now;
                s.lru.remove(&old);
                s.lru.insert(now, id.clone());
            }
            resources::hit("asset_metadata", 1);
        } else {
            resources::miss("asset_metadata");
        }
        found
    }
    pub fn get_mut(&mut self, id: &AssetId) -> Option<&mut RegisteredAsset> {
        self.values.get_mut(id)
    }
    pub fn age(&self, id: &AssetId) -> Option<std::time::Duration> {
        self.tracked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .get(id)
            .map(|e| e.created.elapsed())
    }
    pub fn insert(&mut self, id: AssetId, asset: RegisteredAsset) -> Option<RegisteredAsset> {
        let remote = asset
            .source
            .as_ref()
            .and_then(|s| s.remote.as_ref())
            .filter(|r| r.local_path.is_none());
        if let Some(remote) = remote {
            let bytes = 4096
                + id.0.len() * 2
                + asset.manifest.quality.cumulative_layer_bytes.capacity() * 8;
            let hold = remote.metadata_cost.retain();
            let mut s = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(old) = s.entries.remove(&id) {
                s.bytes -= old.bytes;
                s.lru.remove(&old.tick);
            }
            s.clock += 1;
            let tick = s.clock;
            s.bytes += bytes;
            s.lru.insert(tick, id.clone());
            s.entries.insert(
                id.clone(),
                Entry {
                    bytes,
                    tick,
                    created: Instant::now(),
                    _hold: Some(hold),
                },
            );
        }
        let old = self.values.insert(id, asset);
        self.trim(resources::effective(
            "asset_metadata",
            resources::budget("asset_metadata"),
        ));
        old
    }
    pub fn retain(&mut self, mut f: impl FnMut(&AssetId, &mut RegisteredAsset) -> bool) {
        self.values.retain(|k, v| f(k, v));
        let mut s = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
        let removed = s
            .entries
            .keys()
            .filter(|id| !self.values.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(e) = s.entries.remove(&id) {
                s.bytes -= e.bytes;
                s.lru.remove(&e.tick);
            }
        }
    }
    pub fn trim(&mut self, limit: usize) -> resources::Occupancy {
        let mut s = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
        // The API's registration handoff needs a bounded set of in-flight aliases even
        // with caches disabled. This small runtime floor is part of runtime headroom.
        let floor = 8192 * resources::plan().source_io_concurrency.max(1);
        let ceiling = limit.max(floor);
        while s.bytes.saturating_add(OWNED.load(Ordering::Acquire)) > ceiling && s.entries.len() > 2
        {
            let Some((_, id)) = s.lru.pop_first() else {
                break;
            };
            if let Some(e) = s.entries.remove(&id) {
                s.bytes -= e.bytes;
                self.values.remove(&id);
                resources::evict("asset_metadata", 1);
            }
        }
        resources::Occupancy {
            bytes: s.bytes + OWNED.load(Ordering::Acquire),
            entries: s.entries.len(),
            ..Default::default()
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Locator {
    identity: String,
    source: String,
    key: String,
    checksum: String,
}
fn location() -> std::path::PathBuf {
    std::env::var_os("RIVER_ASSET_LOCATOR_CACHE_ROOT")
        .map(Into::into)
        .unwrap_or_else(|| {
            std::env::var_os("RIVER_S3_CACHE_ROOT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::temp_dir().join("river-s3-cache"))
                .join("asset-locators-v1")
        })
}
fn inventory() -> &'static Mutex<Option<crate::cache_inventory::Inventory>> {
    static I: std::sync::OnceLock<Mutex<Option<crate::cache_inventory::Inventory>>> =
        std::sync::OnceLock::new();
    I.get_or_init(|| Mutex::new(None))
}
fn valid_id(id: &str) -> Option<&str> {
    id.strip_prefix("s3v-")
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn register_locator() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        resources::register("asset_locator_disk", move |_| {
            let i = inventory().lock().unwrap_or_else(|e| e.into_inner());
            resources::Occupancy {
                bytes: i.as_ref().map_or(0, |i| i.bytes as usize),
                entries: i.as_ref().map_or(0, |i| i.entries.len()),
                persistence_location: Some(location().to_string_lossy().into_owned()),
                ..Default::default()
            }
        })
    });
}
fn checksum(id: &str, source: &str, key: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(
        serde_json::to_vec(&(id, source, key)).unwrap(),
    ))
}
fn initialize_inventory(
    state: &mut Option<crate::cache_inventory::Inventory>,
    budget: usize,
) -> anyhow::Result<()> {
    if state.is_none() {
        *state = Some(crate::cache_inventory::Inventory::open(
            location(),
            budget as u64,
            resources::entry_limit("RIVER_ASSET_LOCATOR_CACHE_ENTRIES", budget),
            |p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.len() == 64 && n.bytes().all(|b| b.is_ascii_hexdigit()))
            },
        )?);
    }
    Ok(())
}

pub fn warm_locator_inventory() -> anyhow::Result<()> {
    let budget = resources::budget("asset_locator_disk");
    if budget == 0 {
        return Ok(());
    }
    register_locator();
    let mut state = inventory().lock().unwrap_or_else(|error| error.into_inner());
    initialize_inventory(&mut state, budget)
}

pub fn remember_locator(id: &str, source: &str, key: &str) -> anyhow::Result<()> {
    use std::io::Write;
    let Some(name) = valid_id(id) else {
        return Ok(());
    };
    let budget = resources::budget("asset_locator_disk");
    if !crate::storage_mode::disk_cache_enabled() {
        crate::storage_mode::remember_locator(id, source, key);
        return Ok(());
    }
    if budget == 0 {
        return Ok(());
    }
    register_locator();
    let record = Locator {
        identity: id.into(),
        source: source.into(),
        key: key.into(),
        checksum: checksum(id, source, key),
    };
    let bytes = serde_json::to_vec(&record)?;
    anyhow::ensure!(
        bytes.len() <= 8192 && bytes.len() <= budget,
        "asset locator exceeds cache admission limit"
    );
    let mut state = inventory().lock().unwrap_or_else(|e| e.into_inner());
    initialize_inventory(&mut state, budget)?;
    let i = state.as_mut().unwrap();
    let evicted = i.make_room(name, bytes.len() as u64)?;
    resources::evict("asset_locator_disk", evicted);
    let mut temp = tempfile::NamedTempFile::new_in(&i.root)?;
    temp.write_all(&bytes)?;
    temp.flush()?;
    temp.persist(i.root.join(name))?;
    i.record(name.into(), bytes.len() as u64);
    Ok(())
}
pub fn lookup_locator(id: &str) -> Option<(String, String)> {
    if !crate::storage_mode::disk_cache_enabled() { return crate::storage_mode::lookup_locator(id); }
    use std::io::Read;
    let name = valid_id(id)?;
    if resources::budget("asset_locator_disk") == 0 {
        return None;
    }
    register_locator();
    let result = (|| {
        let path = location().join(name);
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > 8192 {
            return None;
        }
        let mut bytes = vec![];
        std::fs::File::open(path)
            .ok()?
            .take(8193)
            .read_to_end(&mut bytes)
            .ok()?;
        let l: Locator = serde_json::from_slice(&bytes).ok()?;
        if l.identity != id || l.checksum != checksum(&l.identity, &l.source, &l.key) {
            return None;
        }
        Some((l.source, l.key))
    })();
    if result.is_some() {
        resources::hit("asset_locator_disk", 1);
        if let Some(i) = inventory()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            i.touch(name);
        }
    } else {
        resources::miss("asset_locator_disk");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutable_metadata_is_charged_once_across_aliases_and_released() {
        let baseline = OWNED.load(Ordering::Acquire);
        let cost = Arc::new(Cost::default());
        cost.resize(8192);
        let a = cost.retain();
        let b = cost.retain();
        assert_eq!(OWNED.load(Ordering::Acquire), baseline + 8192);
        cost.resize(32768);
        assert_eq!(OWNED.load(Ordering::Acquire), baseline + 32768);
        drop(a);
        assert_eq!(OWNED.load(Ordering::Acquire), baseline + 32768);
        drop(b);
        assert_eq!(OWNED.load(Ordering::Acquire), baseline);
    }
    #[test]
    fn public_version_ids_stay_in_the_existing_api_contract() {
        let id = format!("s3v-{}", "a".repeat(64));
        assert!(crate::AssetId(id.clone()).validate().is_ok());
        assert!(valid_id(&id).is_some());
        assert!(valid_id("s3v-../outside").is_none());
        assert_ne!(
            checksum(&id, "source", "one"),
            checksum(&id, "source", "two")
        );
    }
}
