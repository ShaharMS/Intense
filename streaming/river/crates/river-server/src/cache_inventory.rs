//! Local disposable-cache inventory: one startup scan, O(log n) LRU updates.
//! It never enumerates a bucket or reads image objects.
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};
pub struct Inventory {
    pub root: PathBuf,
    pub bytes: u64,
    pub entries: HashMap<String, (u64, u64)>,
    lru: BTreeMap<u64, String>,
    clock: u64,
    pub budget: u64,
    pub max_entries: usize,
}
impl Inventory {
    pub fn open(
        root: PathBuf,
        budget: u64,
        max_entries: usize,
        owned: fn(&Path) -> bool,
    ) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut found = vec![];
        for entry in fs::read_dir(&root)?.flatten() {
            let path = entry.path();
            if !owned(&path) {
                continue;
            }
            if let Ok(meta) = fs::symlink_metadata(path) {
                if meta.file_type().is_file() {
                    found.push((
                        meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        entry.file_name().to_string_lossy().into_owned(),
                        meta.len(),
                    ));
                }
            }
        }
        found.sort_by_key(|r| r.0);
        let mut me = Self {
            root,
            bytes: 0,
            entries: HashMap::new(),
            lru: BTreeMap::new(),
            clock: 0,
            budget,
            max_entries,
        };
        for (_, name, size) in found {
            me.record(name, size);
        }
        me.make_room("", 0)?;
        Ok(me)
    }
    pub fn touch(&mut self, name: &str) {
        if let Some((_, old)) = self.entries.get_mut(name) {
            self.lru.remove(old);
            self.clock += 1;
            *old = self.clock;
            self.lru.insert(*old, name.into());
        }
    }
    pub fn record(&mut self, name: String, bytes: u64) {
        self.remove(&name);
        self.clock += 1;
        self.bytes += bytes;
        self.lru.insert(self.clock, name.clone());
        self.entries.insert(name, (bytes, self.clock));
    }
    pub fn remove(&mut self, name: &str) {
        if let Some((bytes, tick)) = self.entries.remove(name) {
            self.bytes -= bytes;
            self.lru.remove(&tick);
        }
    }
    pub fn make_room(&mut self, target: &str, extra: u64) -> std::io::Result<usize> {
        let mut removed = 0;
        loop {
            let old = self.entries.get(target).map_or(0, |v| v.0);
            let count = self.entries.len()
                + usize::from(!target.is_empty() && !self.entries.contains_key(target));
            if self.bytes.saturating_sub(old).saturating_add(extra) <= self.budget
                && count <= self.max_entries
            {
                break;
            }
            let victim = self
                .lru
                .iter()
                .find(|(_, n)| n.as_str() != target)
                .map(|(_, n)| n.clone());
            let Some(name) = victim else {
                break;
            };
            match fs::remove_file(self.root.join(&name)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            self.remove(&name);
            removed += 1;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn six_thousand_local_records_survive_restart_then_evict_in_lru_order() {
        let root = tempfile::tempdir().unwrap();
        let owned = |p: &Path| p.extension().is_some_and(|x| x == "json");
        fs::write(root.path().join("keep.txt"), b"not a cache record").unwrap();
        let mut cache = Inventory::open(root.path().into(), 8 * 1024 * 1024, 10000, owned).unwrap();
        for i in 0..6000 {
            let name = format!("{i:06}.json");
            let data = format!("{{\"object\":{i},\"version\":1}}");
            cache.make_room(&name, data.len() as u64).unwrap();
            fs::write(root.path().join(&name), &data).unwrap();
            cache.record(name, data.len() as u64);
        }
        assert_eq!(cache.entries.len(), 6000);
        drop(cache);
        let mut cache = Inventory::open(root.path().into(), 8 * 1024 * 1024, 10000, owned).unwrap();
        assert_eq!(cache.entries.len(), 6000);
        cache.touch("000000.json");
        cache.budget = 1024;
        let n = cache.make_room("", 0).unwrap();
        assert!(n > 4096);
        assert!(cache.bytes <= 1024);
        assert!(root.path().join("000000.json").exists());
        assert!(root.path().join("keep.txt").exists());
    }
}
