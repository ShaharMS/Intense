//! Bounded lossless HTJ2K derivative cache. Only validated compressed outputs
//! are admitted; original inputs and RGB16 build files are temporary.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock, Weak, mpsc},
    time::Instant,
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Params {
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub resolutions: u32,
}
#[derive(Clone, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    key: String,
    sha256: String,
    bytes: u64,
    params: Params,
    preparation_ms: f64,
}
struct Entry {
    manifest: Manifest,
    access: u64,
    data: Weak<Vec<u8>>,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    pending: HashSet<String>,
    tick: u64,
    hits: u64,
    misses: u64,
    failures: u64,
    preparation_ms: f64,
    last_error: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub budget_bytes: u64,
    pub used_bytes: u64,
    pub entries: usize,
    pub pending: usize,
    pub hits: u64,
    pub misses: u64,
    pub failures: u64,
    pub preparation_ms: f64,
    pub last_error: Option<String>,
}
struct Build {
    key: String,
    input: Arc<Vec<u8>>,
    params: Params,
}
pub struct Cache {
    root: PathBuf,
    budget: u64,
    state: Mutex<State>,
    sender: mpsc::SyncSender<Build>,
}
static CACHE: OnceLock<std::result::Result<Arc<Cache>, String>> = OnceLock::new();
pub fn global() -> Result<Arc<Cache>> {
    anyhow::ensure!(crate::storage_mode::disk_cache_enabled() && !crate::adaptation::conserve_disk(), "Persistent HT cache is disabled by the active storage policy");
    CACHE
        .get_or_init(|| Cache::new().map_err(|e| e.to_string()))
        .clone()
        .map_err(anyhow::Error::msg)
}
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn raw_codestream(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.starts_with(&[0xff, 0x4f]) {
        return Ok(bytes.to_vec());
    }
    let mut pos = 0;
    while pos + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[pos..pos + 4].try_into()?) as usize;
        ensure!(
            length >= 8 && pos + length <= bytes.len(),
            "invalid HTJ2K box"
        );
        if &bytes[pos + 4..pos + 8] == b"jp2c" {
            return Ok(bytes[pos + 8..pos + length].to_vec());
        }
        pos += length;
    }
    bail!("HTJ2K codestream box is missing")
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(budget: u64) -> (tempfile::TempDir, Cache) {
        let root = tempfile::tempdir().unwrap();
        let (sender, _) = mpsc::sync_channel(1);
        let cache = Cache {
            root: root.path().into(),
            budget,
            state: Mutex::new(State::default()),
            sender,
        };
        (root, cache)
    }
    fn insert(cache: &Cache, data: &[u8], access: u64) -> String {
        let key = digest(data);
        fs::write(cache.root.join(format!("{key}.j2k")), data).unwrap();
        let manifest = Manifest {
            version: 1,
            key: key.clone(),
            sha256: digest(data),
            bytes: data.len() as u64,
            params: Params {
                width: 1,
                height: 1,
                tile_width: 1,
                tile_height: 1,
                resolutions: 1,
            },
            preparation_ms: 0.0,
        };
        fs::write(
            cache.root.join(format!("{key}.json")),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        cache.state.lock().unwrap().entries.insert(
            key.clone(),
            Entry {
                manifest,
                access,
                data: Weak::new(),
            },
        );
        key
    }
    #[test]
    fn rejects_corrupted_compressed_data() {
        let (_root, cache) = fixture(32);
        let key = insert(&cache, b"original", 1);
        fs::write(cache.root.join(format!("{key}.j2k")), b"modified").unwrap();
        assert!(cache.get(&key).is_none());
        assert_eq!(cache.status().failures, 1);
    }
    #[test]
    fn evicts_least_recent_to_budget() {
        let (_root, cache) = fixture(8);
        let older = insert(&cache, b"123456", 1);
        let newer = insert(&cache, b"abcdef", 2);
        cache.evict(&mut cache.state.lock().unwrap(), 0).unwrap();
        assert!(!cache.root.join(format!("{older}.j2k")).exists());
        assert!(cache.get(&newer).is_some());
        assert!(cache.status().used_bytes <= 8);
    }
    #[test]
    fn zero_budget_disables_admission() {
        let (_root, cache) = fixture(0);
        cache.schedule(
            "a".repeat(64),
            Arc::new(vec![1]),
            Params {
                width: 1,
                height: 1,
                tile_width: 1,
                tile_height: 1,
                resolutions: 1,
            },
        );
        assert_eq!(cache.status().pending, 0);
        assert_eq!(cache.status().misses, 0);
    }
    #[test]
    fn validates_dimensions_and_binary_separator() {
        let (root, _) = fixture(0);
        let path = root.path().join("pixels.ppm");
        fs::write(&path, b"P6\n1 1\n65535\n\x0a\x00\x00\x01\x00\x02").unwrap();
        let pixels = ppm_pixels(&path).unwrap();
        assert_eq!(pixels, (1, 1, vec![10, 0, 0, 1, 0, 2]));
        fs::write(&path, b"P6\n2 1\n65535\n123456").unwrap();
        assert!(ppm_pixels(&path).is_err());
    }
}
fn ppm_pixels(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let data = fs::read(path)?;
    let mut pos = 0;
    let mut tokens = Vec::new();
    while tokens.len() < 4 {
        while data.get(pos).is_some_and(|b| b.is_ascii_whitespace()) {
            pos += 1;
        }
        if data.get(pos) == Some(&b'#') {
            while data.get(pos) != Some(&b'\n') {
                ensure!(pos < data.len(), "short PPM comment");
                pos += 1;
            }
            continue;
        }
        let begin = pos;
        while data.get(pos).is_some_and(|b| !b.is_ascii_whitespace()) {
            pos += 1;
        }
        ensure!(begin < pos, "short PPM header");
        tokens.push(data[begin..pos].to_vec());
    }
    ensure!(
        tokens[0] == b"P6" && tokens[3] == b"65535",
        "HT cache requires exact RGB16 PPM"
    );
    let width = std::str::from_utf8(&tokens[1])?.parse::<u32>()?;
    let height = std::str::from_utf8(&tokens[2])?.parse::<u32>()?;
    ensure!(
        data.get(pos).is_some_and(|b| b.is_ascii_whitespace()),
        "missing PPM separator"
    );
    pos += 1;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(6))
        .context("PPM size overflow")?;
    ensure!(
        data.len().saturating_sub(pos) == expected,
        "invalid PPM raster length"
    );
    Ok((width, height, data[pos..].to_vec()))
}
fn run(program: &str, args: &[String]) -> Result<()> {
    let result = Command::new("nice")
        .args(["-n", "10", program])
        .args(args)
        .output()
        .with_context(|| format!("start {program}"))?;
    ensure!(
        result.status.success(),
        "{program} failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}
impl Cache {
    fn new() -> Result<Arc<Self>> {
        let root = std::env::var_os("RIVER_HT_CACHE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/lib/river/s3-cache/ht-tiles"));
        let budget = std::env::var("RIVER_HT_CACHE_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0u64);
        fs::create_dir_all(&root)?;
        let mut state = State::default();
        for file in fs::read_dir(&root)? {
            let file = file?;
            if !file.file_type()?.is_file()
                || file.path().extension().and_then(|x| x.to_str()) != Some("json")
            {
                continue;
            }
            if let Ok(manifest) = serde_json::from_slice::<Manifest>(&fs::read(file.path())?) {
                let filename = format!("{}.json", manifest.key);
                let output = root.join(format!("{}.j2k", manifest.key));
                if manifest.version == 1
                    && manifest.key.len() == 64
                    && manifest.key.bytes().all(|b| b.is_ascii_hexdigit())
                    && file.file_name().to_str() == Some(filename.as_str())
                    && fs::symlink_metadata(&output)
                        .is_ok_and(|m| m.file_type().is_file() && m.len() == manifest.bytes)
                {
                    state.tick += 1;
                    state.entries.insert(
                        manifest.key.clone(),
                        Entry {
                            manifest,
                            access: state.tick,
                            data: Weak::new(),
                        },
                    );
                }
            }
        }
        let (sender, receiver) = mpsc::sync_channel::<Build>(64);
        let cache = Arc::new(Self {
            root,
            budget,
            state: Mutex::new(state),
            sender,
        });
        {
            let mut state = cache.state.lock().unwrap();
            cache.evict(&mut state, 0)?;
        }
        let receiver = Arc::new(Mutex::new(receiver));
        let workers = std::env::var("RIVER_HT_CACHE_WORKERS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 4);
        for index in 0..workers {
            let owner = Arc::downgrade(&cache);
            let rx = receiver.clone();
            std::thread::Builder::new()
                .name(format!("river-ht-cache-{index}"))
                .spawn(move || {
                    loop {
                        let build = match rx.lock().unwrap().recv() {
                            Ok(build) => build,
                            Err(_) => break,
                        };
                        let Some(cache) = owner.upgrade() else { break };
                        let started = Instant::now();
                        let outcome = cache.prepare(&build);
                        let mut state = cache.state.lock().unwrap();
                        state.pending.remove(&build.key);
                        state.preparation_ms += started.elapsed().as_secs_f64() * 1000.0;
                        if let Err(error) = outcome {
                            state.failures += 1;
                            state.last_error = Some(error.to_string());
                            tracing::warn!(%error,"HT cache preparation failed");
                        }
                    }
                })?;
        }
        Ok(cache)
    }
    fn evict(&self, state: &mut State, additional: u64) -> Result<()> {
        while state
            .entries
            .values()
            .map(|e| e.manifest.bytes)
            .sum::<u64>()
            .saturating_add(additional)
            > self.budget
        {
            let Some(key) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.access)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            for ext in ["j2k", "json"] {
                let file = self.root.join(format!("{key}.{ext}"));
                match fs::remove_file(file) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
            state.entries.remove(&key);
        }
        Ok(())
    }
    pub fn status(&self) -> Status {
        let s = self.state.lock().unwrap();
        Status {
            budget_bytes: self.budget,
            used_bytes: s.entries.values().map(|e| e.manifest.bytes).sum(),
            entries: s.entries.len(),
            pending: s.pending.len(),
            hits: s.hits,
            misses: s.misses,
            failures: s.failures,
            preparation_ms: s.preparation_ms,
            last_error: s.last_error.clone(),
        }
    }
    pub fn get(&self, key: &str) -> Option<Arc<Vec<u8>>> {
        if self.budget == 0 {
            return None;
        }
        let record = {
            let mut state = self.state.lock().unwrap();
            state.tick += 1;
            let tick = state.tick;
            let entry = state.entries.get_mut(key)?;
            entry.access = tick;
            if let Some(data) = entry.data.upgrade() {
                state.hits += 1;
                return Some(data);
            }
            entry.manifest.clone()
        };
        // Read/checksum outside the catalogue mutex; unrelated cache hits remain fast.
        let path = self.root.join(format!("{key}.j2k"));
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(error) => {
                let mut state = self.state.lock().unwrap();
                state.entries.remove(key);
                state.failures += 1;
                state.last_error = Some(format!("cached derivative unavailable: {error}"));
                return None;
            }
        };
        if data.len() as u64 != record.bytes || digest(&data) != record.sha256 {
            let mut s = self.state.lock().unwrap();
            s.entries.remove(key);
            s.failures += 1;
            s.last_error = Some("cached HT derivative checksum mismatch".into());
            return None;
        }
        let data = Arc::new(data);
        let mut s = self.state.lock().unwrap();
        if let Some(entry) = s.entries.get_mut(key) {
            entry.data = Arc::downgrade(&data);
        }
        s.hits += 1;
        Some(data)
    }
    pub fn schedule(&self, key: String, input: Arc<Vec<u8>>, params: Params) {
        if self.budget == 0 || input.len() as u64 > self.budget {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.misses += 1;
        if state.entries.contains_key(&key) || !state.pending.insert(key.clone()) {
            return;
        }
        if self
            .sender
            .try_send(Build {
                key: key.clone(),
                input,
                params,
            })
            .is_err()
        {
            state.pending.remove(&key);
        }
    }
    fn prepare(&self, build: &Build) -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix(".build-")
            .tempdir_in(&self.root)?;
        let input = temp.path().join("original.j2k");
        let raw = temp.path().join("original.ppm");
        let output = temp.path().join("optimized.jph");
        fs::write(&input, build.input.as_ref())?;
        let text = |p: &Path| p.to_string_lossy().into_owned();
        let started = Instant::now();
        run(
            "grk_decompress",
            &[
                "-i".into(),
                text(&input),
                "-o".into(),
                text(&raw),
                "-H".into(),
                "1".into(),
            ],
        )?;
        run(
            "grk_compress",
            &[
                "-i".into(),
                text(&raw),
                "-o".into(),
                text(&output),
                "-n".into(),
                build.params.resolutions.to_string(),
                "-t".into(),
                format!("{},{}", build.params.tile_width, build.params.tile_height),
                "-b".into(),
                "64,64".into(),
                "-Y".into(),
                "0".into(),
                "-M".into(),
                "64".into(),
                "-Z".into(),
                "16384".into(),
                "-H".into(),
                "1".into(),
            ],
        )?;
        let validation = temp.path().join("validation.ppm");
        run(
            "grk_decompress",
            &[
                "-i".into(),
                text(&output),
                "-o".into(),
                text(&validation),
                "-H".into(),
                "1".into(),
            ],
        )?;
        let original = ppm_pixels(&raw)?;
        ensure!(
            (original.0, original.1) == (build.params.width, build.params.height),
            "HT input dimensions mismatch"
        );
        ensure!(
            original == ppm_pixels(&validation)?,
            "HT RGB16 pixel verification failed"
        );
        for reduction in [2, build.params.resolutions - 1] {
            let before = temp.path().join(format!("before-{reduction}.ppm"));
            let after = temp.path().join(format!("after-{reduction}.ppm"));
            for (source, target) in [(&input, &before), (&output, &after)] {
                run(
                    "grk_decompress",
                    &[
                        "-i".into(),
                        text(source),
                        "-o".into(),
                        text(target),
                        "-r".into(),
                        reduction.to_string(),
                        "-H".into(),
                        "1".into(),
                    ],
                )?;
            }
            ensure!(
                ppm_pixels(&before)? == ppm_pixels(&after)?,
                "HT reduced RGB16 verification failed at {reduction}"
            );
        }
        let data = raw_codestream(&fs::read(&output)?)?;
        ensure!(
            data.len() as u64 <= self.budget,
            "HT derivative exceeds cache budget"
        );
        let manifest = Manifest {
            version: 1,
            key: build.key.clone(),
            sha256: digest(&data),
            bytes: data.len() as u64,
            params: build.params.clone(),
            preparation_ms: started.elapsed().as_secs_f64() * 1000.0,
        };
        let staged = temp.path().join("validated.j2k");
        let metadata = temp.path().join("validated.json");
        fs::write(&staged, &data)?;
        fs::write(&metadata, serde_json::to_vec(&manifest)?)?;
        let mut state = self.state.lock().unwrap();
        self.evict(&mut state, manifest.bytes)?;
        fs::rename(staged, self.root.join(format!("{}.j2k", build.key)))?;
        fs::rename(metadata, self.root.join(format!("{}.json", build.key)))?;
        state.tick += 1;
        let access = state.tick;
        state.entries.insert(
            build.key.clone(),
            Entry {
                manifest,
                access,
                data: Weak::new(),
            },
        );
        Ok(())
    }
}
