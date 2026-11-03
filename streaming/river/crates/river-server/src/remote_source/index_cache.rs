//! Disposable version-keyed technical metadata; never a source-file rewrite.
use super::{Header, PartIndex, TilePart};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, sync::{Mutex,OnceLock}, collections::HashMap, time::{Instant,Duration}};

const MAX_FILE: u64 = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_SCAN_ENTRIES: usize = MAX_ENTRIES * 2;
static WRITER: Mutex<()> = Mutex::new(());

struct HotEntry { index:PartIndex, signature:([u8;32],u64), bytes:usize, touched:u64, saved:usize, saved_complete:bool, checkpoint:Instant }
#[derive(Default)] struct HotState { entries:HashMap<String,HotEntry>,bytes:usize,clock:u64 }
struct Hot {limit:usize,state:Mutex<HotState>}
fn hot()->&'static Hot {
    static HOT:OnceLock<Hot>=OnceLock::new();
    HOT.get_or_init(||Hot {limit:std::env::var("RIVER_S3_INDEX_MEMORY_BYTES").ok().and_then(|v|v.parse().ok()).unwrap_or(32*1024*1024),state:Mutex::new(HotState::default())})
}
// Publish validated prefixes only in the existing bounded metadata cache.
// Readers can finish an early tile while another request discovers later offsets.
pub(super) fn publish(identity:&str,index:&PartIndex,header:&Header,size:u64) {
    hot().put(identity,index,(header.payload_digest,size),false,false);
}
pub(super) fn ready(identity:&str,header:&Header,size:u64,selected:&std::collections::HashSet<u16>)->Option<Vec<TilePart>> {
    let mut state=hot().state.lock().unwrap_or_else(|e|e.into_inner());
    state.clock+=1;let clock=state.clock;
    let entry=state.entries.get_mut(identity)?;
    if entry.signature!=(header.payload_digest,size) {return None;}
    entry.touched=clock;
    super::ready_parts(&entry.index,selected)
}
impl Hot {
    fn get(&self,id:&str,signature:([u8;32],u64))->Option<PartIndex> {
        let mut state=self.state.lock().unwrap_or_else(|e|e.into_inner());state.clock+=1;let clock=state.clock;
        let entry=state.entries.get_mut(id)?;if entry.signature!=signature {return None;}
        entry.touched=clock;Some(entry.index.clone())
    }
    fn put(&self,id:&str,index:&PartIndex,signature:([u8;32],u64),loaded:bool,force:bool)->(bool,bool) {
        let bytes=index.parts.capacity()*std::mem::size_of::<super::TilePart>()+id.len()+256;
        if bytes>1024*1024 || bytes>self.limit {return (false,true);}
        let mut state=self.state.lock().unwrap_or_else(|e|e.into_inner());
        if let Some(old)=state.entries.get(id) {if old.signature==signature && old.index.parts.len()>index.parts.len() {return (true,false);}}
        let prior=state.entries.remove(id);if let Some(old)=&prior {state.bytes-=old.bytes;}
        let (saved,saved_complete,checkpoint)=prior.filter(|old|old.signature==signature).map(|old|(old.saved,old.saved_complete,old.checkpoint))
            .unwrap_or((if loaded {index.parts.len()} else {0},if loaded {index.complete} else {false},Instant::now()));
        let changed=index.parts.len()!=saved || index.complete!=saved_complete;
        let write=changed && (force || saved==0 || index.parts.len().saturating_sub(saved)>=128 || checkpoint.elapsed()>=Duration::from_secs(1));
        while state.bytes+bytes>self.limit || state.entries.len()>=MAX_ENTRIES {
            let Some(key)=state.entries.iter().min_by_key(|(_,e)|e.touched).map(|(key,_)|key.clone()) else {break;};
            let old=state.entries.remove(&key).unwrap();state.bytes-=old.bytes;
        }
        state.clock+=1;let touched=state.clock;state.bytes+=bytes;
        state.entries.insert(id.into(),HotEntry {index:index.clone(),signature,bytes,touched,saved,saved_complete,checkpoint});
        (true,write)
    }
    fn saved(&self,id:&str,count:usize,complete:bool) {
        if let Some(entry)=self.state.lock().unwrap_or_else(|e|e.into_inner()).entries.get_mut(id) {entry.saved=count;entry.saved_complete=complete;entry.checkpoint=Instant::now();}
    }
}


#[derive(serde::Serialize, serde::Deserialize)]
struct Record { version: u8, identity: String, index: PartIndex, checksum: String }

fn settings() -> (PathBuf, u64) {
    let root = std::env::var_os("RIVER_S3_INDEX_CACHE_ROOT").map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("RIVER_S3_CACHE_ROOT").map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("river-s3-cache")).join("technical-index"));
    let budget = std::env::var("RIVER_S3_INDEX_CACHE_BYTES").ok().and_then(|value| value.parse().ok())
        .unwrap_or(32 * 1024 * 1024u64).min(512 * 1024 * 1024);
    (root, budget)
}
fn filename(identity: &str) -> String { format!("{}.json", hex::encode(Sha256::digest(identity.as_bytes()))) }
fn checksum(index: &PartIndex) -> Result<String> { Ok(hex::encode(Sha256::digest(serde_json::to_vec(index)?))) }

fn validate(record: &Record, identity: &str, first: u64, size: u64, tile_count: u64) -> Result<()> {
    ensure!(record.version == 1 && record.identity == identity, "index snapshot mismatch");
    ensure!(record.index.parts.len() <= 100_000 && record.checksum == checksum(&record.index)?, "invalid index checksum/count");
    let mut cursor = first;
    for part in &record.index.parts {
        ensure!(part.offset == cursor && part.length >= 14 && u64::from(part.tile) < tile_count,
            "invalid indexed tile extent");
        ensure!(part.total == 0 || part.ordinal < part.total, "invalid tile-part ordinal");
        cursor = cursor.checked_add(u64::from(part.length)).ok_or_else(|| anyhow::anyhow!("index offset overflow"))?;
        ensure!(cursor <= size.saturating_sub(2), "indexed extent exceeds source");
    }
    ensure!(cursor == record.index.cursor, "invalid index cursor");
    Ok(())
}

pub(super) fn load(identity: &str, header: &Header, size: u64) -> Option<PartIndex> {
    let signature=(header.payload_digest,size);
    if let Some(index)=hot().get(identity,signature) {return Some(index);}
    let (root, budget) = settings();
    if budget == 0 { return None; }
    let path = root.join(filename(identity));
    let index=load_from(&path, budget, identity, header.first_part, size,
        u64::from(header.layout.columns) * u64::from(header.layout.rows))?;
    hot().put(identity,&index,signature,true,false);
    Some(index)
}
fn load_from(path: &Path, budget: u64, identity: &str, first: u64, size: u64, tile_count: u64) -> Option<PartIndex> {
    if budget == 0 { return None; }
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE.min(budget) { return None; }
    let mut bytes = vec![];
    fs::File::open(path).ok()?.take(MAX_FILE + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_FILE { return None; }
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    validate(&record, identity, first, size, tile_count).ok()?;
    Some(record.index)
}

fn owned(path: &Path) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some("json")
        && path.file_stem().and_then(|value| value.to_str()).is_some_and(|stem|
            stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
pub(super) fn save(identity: &str, index: &PartIndex, header:&Header, size:u64, force:bool) -> Result<bool> {
    let (memory,write)=hot().put(identity,index,(header.payload_digest,size),false,force);
    let (root, budget) = settings();
    if budget == 0 { return Ok(memory); }
    if write {
        if let Err(error)=save_to(&root,budget,identity,index) {
            if memory {tracing::debug!(%error,"technical index retained in bounded memory");return Ok(true);} else {return Err(error);}
        }
        hot().saved(identity,index.parts.len(),index.complete);
    }
    Ok(true)
}

fn save_to(root: &Path, budget: u64, identity: &str, index: &PartIndex) -> Result<()> {
    if budget == 0 { return Ok(()); }
    let record = Record { version: 1, identity: identity.into(), index: index.clone(), checksum: checksum(index)? };
    let bytes = serde_json::to_vec(&record)?;
    ensure!(bytes.len() as u64 <= MAX_FILE.min(budget), "index exceeds metadata-cache admission limit");
    // Serialize writers to keep the aggregate byte/file limits honest.
    let _writer = WRITER.lock().unwrap_or_else(|error| error.into_inner());
    fs::create_dir_all(&root)?;
    let target = root.join(filename(identity));
    let mut entries = vec![];
    let mut total = 0u64;
    for (position, entry) in fs::read_dir(&root)?.take(MAX_SCAN_ENTRIES+1).enumerate() {
        ensure!(position < MAX_SCAN_ENTRIES, "too many index-cache directory entries");
        let entry = entry?;
        let path = entry.path();
        if !owned(&path) { continue; }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() { continue; }
        if path == target { continue; }
        total = total.saturating_add(metadata.len());
        entries.push((metadata.modified()?, path, metadata.len()));
    }
    ensure!(entries.len() <= MAX_SCAN_ENTRIES, "too many index-cache files");
    entries.sort_by_key(|entry| entry.0);
    let mut remaining = entries.len();
    for (_, path, length) in entries {
        if total.saturating_add(bytes.len() as u64) <= budget && remaining < MAX_ENTRIES { break; }
        fs::remove_file(path)?;
        total = total.saturating_sub(length); remaining -= 1;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    temporary.write_all(&bytes)?; temporary.flush()?;
    temporary.persist(target)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_source::TilePart;
    #[test]
    fn hot_index_budget_identity_and_checkpoint_coalescing() {
        let hot=Hot {limit:8192,state:Mutex::new(HotState::default())};
        let mut index=PartIndex {parts:(0..64).map(|n|TilePart {tile:n,offset:100+u64::from(n)*20,length:20,ordinal:0,total:1}).collect(),cursor:1380,complete:false};
        let signature=([1;32],4096);
        assert_eq!(hot.put("one",&index,signature,false,false),(true,true));hot.saved("one",64,false);
        index.parts.push(TilePart {tile:64,offset:1380,length:20,ordinal:0,total:1});index.cursor=1400;
        assert_eq!(hot.put("one",&index,signature,false,false),(true,false));
        assert_eq!(hot.get("one",signature).unwrap().parts.len(),65);
        assert!(hot.get("one",([2;32],4096)).is_none());
        assert_eq!(hot.put("one",&index,signature,false,true),(true,true));
        for n in 0..16 {hot.put(&n.to_string(),&index,signature,false,false);}
        assert!(hot.state.lock().unwrap().bytes<=8192);
        assert!(hot.get("one",signature).is_none());
    }
    #[test]
    fn working_set_retention_uses_available_bytes_beyond_legacy_entry_cap() {
        let hot=Hot {limit:8*1024*1024,state:Mutex::new(HotState::default())};
        let index=PartIndex {parts:vec![TilePart {tile:0,offset:100,length:20,ordinal:0,total:1}],cursor:120,complete:true};
        let signature=([3;32],122);
        for id in 0..512 {assert!(hot.put(&format!("working-set-{id}"),&index,signature,false,false).0);}
        for id in 0..512 {assert!(hot.get(&format!("working-set-{id}"),signature).is_some(),"index evicted with byte budget available");}
        assert_eq!(hot.state.lock().unwrap().entries.len(),512);
        for id in 512..=MAX_ENTRIES {hot.put(&format!("working-set-{id}"),&index,signature,false,false);}
        let state=hot.state.lock().unwrap();
        assert_eq!(state.entries.len(),MAX_ENTRIES);assert!(state.bytes<=hot.limit);drop(state);
        assert!(hot.get("working-set-0",signature).is_none());
        assert!(hot.get(&format!("working-set-{MAX_ENTRIES}"),signature).is_some());
    }
    #[test]
    fn rejects_wrong_snapshots_corruption_and_impossible_extents() {
        let index = PartIndex { parts: vec![TilePart { tile: 0, offset: 100, length: 20, ordinal: 0, total: 1 }], cursor: 120, complete: false };
        let mut record = Record { version: 1, identity: "snapshot".into(), checksum: checksum(&index).unwrap(), index };
        assert!(validate(&record, "snapshot", 100, 122, 1).is_ok());
        assert!(validate(&record, "new-version", 100, 122, 1).is_err());
        assert!(validate(&record, "snapshot", 100, 119, 1).is_err());
        record.index.parts[0].tile = 1;
        assert!(validate(&record, "snapshot", 100, 122, 2).is_err());
        record.checksum = checksum(&record.index).unwrap();
        assert!(validate(&record, "snapshot", 100, 122, 1).is_err());
    }
    #[test]
    fn disk_round_trip_evicts_only_owned_files_and_ignores_corruption() {
        let root = tempfile::tempdir().unwrap();
        let index = PartIndex { parts: vec![TilePart { tile: 0, offset: 100, length: 20, ordinal: 0, total: 1 }], cursor: 120, complete: false };
        fs::write(root.path().join("keep.txt"), "unrelated").unwrap();
        save_to(root.path(), 1024, "one", &index).unwrap();
        let first = root.path().join(filename("one"));
        let size = fs::metadata(&first).unwrap().len();
        assert_eq!(load_from(&first, 1024, "one", 100, 122, 1).unwrap().parts, index.parts);
        assert!(load_from(&first, 0, "one", 100, 122, 1).is_none());
        assert!(load_from(&first, 1024, "new-version", 100, 122, 1).is_none());
        save_to(root.path(), size + 1, "two", &index).unwrap();
        assert!(!first.exists());
        assert_eq!(fs::read_to_string(root.path().join("keep.txt")).unwrap(), "unrelated");
        let second = root.path().join(filename("two"));
        fs::write(&second, "corrupt").unwrap();
        assert!(load_from(&second, 1024, "two", 100, 122, 1).is_none());
    }
}
