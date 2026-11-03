//! Verified, bounded copies of immutable codestreams on Linux-local storage.
//! This caches source bytes, never decoded tiles or generated pyramids.
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};
use tracing::instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub struct SourceCache {
    root: PathBuf,
    input_root: PathBuf,
    limit: u64,
    state: Mutex<State>,
    pending: Mutex<HashSet<PathBuf>>,
    background: Mutex<()>,
}
struct State {
    files: HashMap<String, Arc<CachedFile>>,
    touched: HashMap<String, u64>,
    clock: u64,
}
pub struct CachedFile {
    pub path: PathBuf,
    bytes: u64,
    digest: String,
    verified: Mutex<bool>,
}

fn digest_file(path: &Path) -> Result<String> {
    let mut reader = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}
fn hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}

impl SourceCache {
    pub fn from_env() -> Result<Option<Self>> {
        let Some(root) = std::env::var_os("RIVER_LOCAL_SOURCE_CACHE_ROOT") else {
            return Ok(None);
        };
        let input_root = PathBuf::from(
            std::env::var_os("RIVER_MOSAIC_ROOT")
                .unwrap_or_else(|| "/app/sentinel-date-mosaics".into()),
        )
        .canonicalize()?;
        let limit = std::env::var("RIVER_LOCAL_SOURCE_CACHE_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2 * 1024 * 1024 * 1024);
        Self::new(PathBuf::from(root), input_root, limit).map(Some)
    }
    fn new(root: PathBuf, input_root: PathBuf, limit: u64) -> Result<Self> {
        fs::create_dir_all(&root)?;
        let root = root.canonicalize()?;
        ensure!(
            !root.starts_with(&input_root) && !input_root.starts_with(&root),
            "source cache must be separate from the original corpus"
        );
        let mut files = HashMap::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(name) = name.strip_prefix("river-source-") else {
                continue;
            };
            let Some(stem) = name
                .strip_suffix(".jp2")
                .or_else(|| name.strip_suffix(".j2k"))
            else {
                continue;
            };
            let Some((key, digest)) = stem.split_once('-') else {
                continue;
            };
            if hex_digest(key) && hex_digest(digest) {
                files.insert(
                    key.to_owned(),
                    Arc::new(CachedFile {
                        path: entry.path(),
                        bytes: entry.metadata()?.len(),
                        digest: digest.to_owned(),
                        verified: Mutex::new(false),
                    }),
                );
            }
        }
        Ok(Self {
            root,
            input_root,
            limit,
            state: Mutex::new(State {
                files,
                touched: HashMap::new(),
                clock: 0,
            }),
            pending: Mutex::new(HashSet::new()),
            background: Mutex::new(()),
        })
    }
    /// Foreground acquisition never copies, hashes, or waits for verification.
    #[instrument(name = "river.source.cache.acquire", skip(self, source), fields(cache_hit = tracing::field::Empty))]
    pub fn ready(&self, source: &Path) -> Result<(Option<Arc<CachedFile>>, bool)> {
        let source = source.canonicalize()?;
        if !source.starts_with(&self.input_root) {
            return Ok((None, false));
        }
        let metadata = fs::metadata(&source)?;
        if metadata.len() > self.limit {
            return Ok((None, false));
        }
        let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
        let key = hex::encode(Sha256::digest(
            format!("{}:{}:{modified}", source.display(), metadata.len()).as_bytes(),
        ));
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("source cache lock poisoned"))?;
        state.clock += 1;
        let clock = state.clock;
        state.touched.insert(key.clone(), clock);
        let file = state.files.get(&key).cloned();
        drop(state);
        let file = file.filter(|file| file.verified.try_lock().is_ok_and(|verified| *verified));
        tracing::Span::current().record("cache_hit", file.is_some());
        Ok((file, true))
    }
    /// Called after raster completion; one bounded background copier, at most
    /// four distinct queued assets. Duplicate cold tiles do not enqueue copies.
    pub fn schedule(self: &Arc<Self>, source: PathBuf) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        if pending.len() >= 4 || !pending.insert(source.clone()) {
            return;
        }
        drop(pending);
        let cache = self.clone();
        // Do not retain the foreground span until background IO finishes.
        let parent = tracing::Span::current().context();
        let cleanup_source = source.clone();
        let spawn = std::thread::Builder::new().name("river-source-cache".into()).spawn(move || {
            let span = tracing::info_span!(parent: None, "river.source.cache.fill", source = %source.display());
            let _ = span.set_parent(parent);
            let _entered = span.enter();
            let _worker = cache.background.lock().unwrap_or_else(|error| error.into_inner());
            if let Err(error) = cache.get(&source) { tracing::warn!(%error, "background source cache fill failed; original remains available"); }
            if let Ok(mut pending) = cache.pending.lock() { pending.remove(&source); }
        });
        if spawn.is_err() {
            if let Ok(mut pending) = self.pending.lock() {
                pending.remove(&cleanup_source);
            }
        }
    }
    #[instrument(name = "river.source.cache.prepare", skip(self, source), fields(cache_hit = tracing::field::Empty, source_bytes = tracing::field::Empty))]
    pub fn get(&self, source: &Path) -> Result<Option<Arc<CachedFile>>> {
        let source = source.canonicalize()?;
        if !source.starts_with(&self.input_root) {
            return Ok(None);
        }
        let metadata = fs::metadata(&source)?;
        let bytes = metadata.len();
        tracing::Span::current().record("source_bytes", bytes);
        if bytes > self.limit {
            return Ok(None);
        }
        let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
        let key = hex::encode(Sha256::digest(
            format!("{}:{bytes}:{modified}", source.display()).as_bytes(),
        ));
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("source cache lock poisoned"))?;
        state.clock += 1;
        let clock = state.clock;
        state.touched.insert(key.clone(), clock);
        if let Some(file) = state.files.get(&key).cloned() {
            drop(state);
            let mut verified = file
                .verified
                .lock()
                .map_err(|_| anyhow::anyhow!("cache verification lock poisoned"))?;
            if !*verified {
                ensure!(
                    digest_file(&file.path)? == file.digest,
                    "cached codestream checksum does not match"
                );
                *verified = true;
            }
            drop(verified);
            tracing::Span::current().record("cache_hit", true);
            return Ok(Some(file));
        }
        tracing::Span::current().record("cache_hit", false);
        let mut used: u64 = state.files.values().map(|file| file.bytes).sum();
        while used + bytes > self.limit {
            // Only our named copies are eligible, and an active decode lease
            // prevents eviction while a CPU/GPU request is reading its path.
            let victim = state
                .files
                .iter()
                .filter(|(_, file)| Arc::strong_count(file) == 1)
                .min_by_key(|(id, _)| state.touched.get(*id).copied().unwrap_or(0))
                .map(|(id, _)| id.clone());
            let Some(victim) = victim else {
                return Ok(None);
            };
            let file = state.files.remove(&victim).unwrap();
            fs::remove_file(&file.path)?;
            used -= file.bytes;
            state.touched.remove(&victim);
        }
        // Release the catalog lock before any bulk IO. Ready assets remain
        // available while another source is copied or revalidated.
        drop(state);
        let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
        let mut input = fs::File::open(&source)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            temporary.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
        }
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        let digest = hex::encode(hash.finalize());
        let after = fs::metadata(&source)?;
        ensure!(
            after.len() == bytes && after.modified()? == metadata.modified()?,
            "source changed while staging"
        );
        ensure!(
            temporary.as_file().metadata()?.len() == bytes
                && digest_file(temporary.path())? == digest,
            "staged source checksum does not match"
        );
        let extension = if source
            .extension()
            .is_some_and(|v| v.eq_ignore_ascii_case("jp2"))
        {
            "jp2"
        } else {
            "j2k"
        };
        let path = self
            .root
            .join(format!("river-source-{key}-{digest}.{extension}"));
        temporary
            .persist(&path)
            .context("unable to publish verified source copy")?;
        let file = Arc::new(CachedFile {
            path,
            bytes,
            digest,
            verified: Mutex::new(true),
        });
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("source cache lock poisoned"))?
            .files
            .insert(key, file.clone());
        Ok(Some(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_lookup_never_waits_for_pending_fill_or_verification() -> Result<()> {
        let input = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let a = input.path().join("a.jp2");
        let b = input.path().join("b.jp2");
        fs::write(&a, b"first")?;
        fs::write(&b, b"other")?;
        let cache = Arc::new(SourceCache::new(
            output.path().to_path_buf(),
            input.path().canonicalize()?,
            20,
        )?);
        let file = cache.get(&a)?.unwrap();
        let worker = cache.background.lock().unwrap();
        cache.schedule(b.clone());
        cache.schedule(b.clone());
        assert_eq!(cache.pending.lock().unwrap().len(), 1);
        assert!(cache.ready(&a)?.0.is_some());
        assert!(cache.ready(&b)?.0.is_none());
        let verification = file.verified.lock().unwrap();
        assert!(cache.ready(&a)?.0.is_none());
        drop(verification);
        drop(worker);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cache.ready(&b)?.0.is_some() {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "background cache fill did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(fs::read(&a)?, b"first");
        Ok(())
    }
    #[test]
    fn verifies_copies_and_preserves_active_leases_during_eviction() -> Result<()> {
        let input = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        fs::write(input.path().join("a.jp2"), b"first")?;
        fs::write(input.path().join("b.jp2"), b"other")?;
        let cache = SourceCache::new(output.path().to_path_buf(), input.path().canonicalize()?, 5)?;
        let first = cache.get(&input.path().join("a.jp2"))?.unwrap();
        assert_eq!(fs::read(&first.path)?, b"first");
        assert!(cache.get(&input.path().join("b.jp2"))?.is_none());
        let old = first.path.clone();
        drop(first);
        let second = cache.get(&input.path().join("b.jp2"))?.unwrap();
        assert!(!old.exists());
        assert_eq!(fs::read(&second.path)?, b"other");
        assert_eq!(fs::read(input.path().join("a.jp2"))?, b"first");
        Ok(())
    }
    #[test]
    fn revalidates_persisted_copies_and_detects_corruption() -> Result<()> {
        let input = tempfile::tempdir()?;
        let output = tempfile::tempdir()?;
        let source = input.path().join("a.jp2");
        fs::write(&source, b"first")?;
        let cache = SourceCache::new(
            output.path().to_path_buf(),
            input.path().canonicalize()?,
            20,
        )?;
        let path = cache.get(&source)?.unwrap().path.clone();
        drop(cache);
        fs::write(&path, b"wrong")?;
        let cache = SourceCache::new(
            output.path().to_path_buf(),
            input.path().canonicalize()?,
            20,
        )?;
        assert!(cache.get(&source).is_err());
        Ok(())
    }
}
