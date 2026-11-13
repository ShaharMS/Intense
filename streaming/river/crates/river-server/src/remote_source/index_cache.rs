//! Disposable version-keyed technical metadata; never a source-file rewrite.
use super::{Header, PartIndex, TilePart};
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, sync::{Mutex,OnceLock}, collections::HashMap, time::{Instant,Duration}};

const MAX_FILE: u64 = 4 * 1024 * 1024;
static WRITER: Mutex<()> = Mutex::new(());
fn entries(bytes:usize)->usize {crate::resources::entry_limit("RIVER_S3_INDEX_MEMORY_ENTRIES",bytes)}
struct HotEntry {index:PartIndex,signature:([u8;32],u64),bytes:usize,touched:u64,saved:usize,saved_complete:bool,checkpoint:Instant}
#[derive(Default)]struct HotState {entries:HashMap<String,HotEntry>,lru:std::collections::BTreeMap<u64,String>,bytes:usize,clock:u64}
struct Hot {limit:usize,state:Mutex<HotState>}
fn hot()->&'static Hot {static HOT:OnceLock<Hot>=OnceLock::new();static REGISTER:std::sync::Once=std::sync::Once::new();let cache=HOT.get_or_init(||Hot{limit:crate::resources::budget("indexes"),state:Mutex::new(HotState::default())});REGISTER.call_once(||crate::resources::register("indexes",move|limit|cache.trim(limit)));cache}
impl Hot {
 fn trim(&self,limit:usize)->crate::resources::Occupancy {let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());while s.bytes>limit||s.entries.len()>entries(limit){let Some((_,key))=s.lru.pop_first()else{break;};if let Some(e)=s.entries.remove(&key){s.bytes-=e.bytes;crate::resources::evict("indexes",1);}}crate::resources::Occupancy{bytes:s.bytes,entries:s.entries.len(),..Default::default()}}
 fn get(&self,id:&str,signature:([u8;32],u64))->Option<PartIndex>{let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());s.clock+=1;let now=s.clock;let Some(e)=s.entries.get_mut(id).filter(|e|e.signature==signature)else{crate::resources::miss("indexes");return None;};let old=e.touched;e.touched=now;let index=e.index.clone();s.lru.remove(&old);s.lru.insert(now,id.into());crate::resources::hit("indexes",index.parts.len());Some(index)}
 fn put(&self,id:&str,index:&PartIndex,signature:([u8;32],u64),loaded:bool,force:bool)->(bool,bool){let limit=crate::resources::effective("indexes",self.limit);let bytes=index.parts.capacity()*std::mem::size_of::<TilePart>()+id.len()+256;if bytes>4*1024*1024||bytes>limit{return(false,true);}let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());if s.entries.get(id).is_some_and(|old|old.signature==signature&&old.index.parts.len()>index.parts.len()){return(true,false);}
  let prior=s.entries.remove(id);if let Some(old)=&prior{s.bytes-=old.bytes;s.lru.remove(&old.touched);}let(saved,saved_complete,checkpoint)=prior.filter(|old|old.signature==signature).map(|old|(old.saved,old.saved_complete,old.checkpoint)).unwrap_or((if loaded{index.parts.len()}else{0},loaded&&index.complete,Instant::now()));let changed=index.parts.len()!=saved||index.complete!=saved_complete;let write=changed&&(force||saved==0||index.parts.len().saturating_sub(saved)>=128||checkpoint.elapsed()>=Duration::from_secs(1));
  while s.bytes+bytes>limit||s.entries.len()>=entries(limit){let Some((_,key))=s.lru.pop_first()else{break;};if let Some(old)=s.entries.remove(&key){s.bytes-=old.bytes;crate::resources::evict("indexes",1);}}
  s.clock+=1;let touched=s.clock;s.bytes+=bytes;s.lru.insert(touched,id.into());s.entries.insert(id.into(),HotEntry{index:index.clone(),signature,bytes,touched,saved,saved_complete,checkpoint});(true,write)
 }
 fn saved(&self,id:&str,count:usize,complete:bool){if let Some(e)=self.state.lock().unwrap_or_else(|e|e.into_inner()).entries.get_mut(id){e.saved=count;e.saved_complete=complete;e.checkpoint=Instant::now();}}
}
pub(super)fn publish(identity:&str,index:&PartIndex,header:&Header,size:u64){hot().put(identity,index,(header.payload_digest,size),false,false);}
pub(super)fn ready(identity:&str,header:&Header,size:u64,selected:&std::collections::HashSet<u16>)->Option<Vec<TilePart>>{super::ready_parts(&hot().get(identity,(header.payload_digest,size))?,selected)}
fn inventories()->&'static Mutex<HashMap<PathBuf,crate::cache_inventory::Inventory>>{static I:OnceLock<Mutex<HashMap<PathBuf,crate::cache_inventory::Inventory>>>=OnceLock::new();I.get_or_init(||Mutex::new(HashMap::new()))}
fn disk_entries(budget:u64)->usize{crate::resources::entry_limit("RIVER_S3_INDEX_CACHE_ENTRIES",budget.min(usize::MAX as u64) as usize)}
fn register_disk(root:&Path,budget:u64){
 static REGISTER:std::sync::Once=std::sync::Once::new();let root=root.to_path_buf();
 REGISTER.call_once(move||{
  if budget>0 {let mut all=inventories().lock().unwrap_or_else(|e|e.into_inner());if let Ok(i)=crate::cache_inventory::Inventory::open(root.clone(),budget,disk_entries(budget),owned){all.insert(root.clone(),i);}}
  crate::resources::register("index_disk",move|_|{let all=inventories().lock().unwrap_or_else(|e|e.into_inner());let i=all.get(&root);crate::resources::Occupancy{bytes:i.map_or(0,|i|i.bytes as usize),entries:i.map_or(0,|i|i.entries.len()),persistence_location:Some(root.to_string_lossy().into_owned()),..Default::default()}});
 });
}


#[derive(serde::Serialize, serde::Deserialize)]
struct Record { version: u8, identity: String, index: PartIndex, checksum: String }

fn settings() -> (PathBuf, u64) {
    let root = std::env::var_os("RIVER_S3_INDEX_CACHE_ROOT").map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("RIVER_S3_CACHE_ROOT").map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("river-s3-cache")).join("technical-index"));
    let budget = crate::resources::budget("index_disk") as u64;
    register_disk(&root,budget);
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
    if let Some(data) = crate::shared_cache::get("index", &filename(identity)) {
        if let Ok(record) = serde_json::from_slice::<Record>(&data) {
            if validate(&record, identity, header.first_part, size,
                        u64::from(header.layout.columns) * u64::from(header.layout.rows)).is_ok() {
                hot().put(identity, &record.index, signature, true, false);
                return Some(record.index);
            }
        }
    }
    let (root, budget) = settings();
    if budget == 0 { return None; }
    let path = root.join(filename(identity));
    let index=load_from(&path, budget, identity, header.first_part, size,
        u64::from(header.layout.columns) * u64::from(header.layout.rows))?;
    crate::resources::hit("index_disk",index.parts.len());
    if let Some(i)=inventories().lock().unwrap_or_else(|e|e.into_inner()).get_mut(&root){i.touch(&filename(identity));}
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
    // A walk can satisfy the requested tiles before it reads EOC. Share the
    // validated contiguous prefix too, while preserving its completeness bit.
    if write && (index.complete || index.parts.len() >= 16)
        && crate::shared_cache::admit("index", index.parts.len().saturating_mul(128).saturating_add(512)) {
        let record = Record { version: 1, identity: identity.into(), index: index.clone(), checksum: checksum(index)? };
        let bytes = serde_json::to_vec(&record)?;
        crate::shared_cache::fill("index", &filename(identity), bytes);
        hot().saved(identity, index.parts.len(), index.complete);
    }
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
    let mut inventories=inventories().lock().unwrap_or_else(|e|e.into_inner());
    if !inventories.contains_key(root) {inventories.insert(root.to_path_buf(),crate::cache_inventory::Inventory::open(root.to_path_buf(),budget,disk_entries(budget),owned)?);}
    let inventory=inventories.get_mut(root).unwrap();inventory.budget=budget;inventory.max_entries=disk_entries(budget);
    let name=filename(identity);let removed=inventory.make_room(&name,bytes.len() as u64)?;crate::resources::evict("index_disk",removed);
    let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
    temporary.write_all(&bytes)?; temporary.flush()?;
    temporary.persist(target)?;
    inventory.record(name,bytes.len() as u64);
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
    fn catalog_churn_exceeds_4096_without_scans_or_premature_eviction() {
        let hot=Hot{limit:16*1024*1024,state:Mutex::new(HotState::default())};
        let index=PartIndex{parts:vec![TilePart{tile:0,offset:100,length:20,ordinal:0,total:1}],cursor:120,complete:true};
        for id in 0..6000 {assert!(hot.put(&format!("object-{id}"),&index,([3;32],122),false,false).0);}
        assert_eq!(hot.state.lock().unwrap().entries.len(),6000);
        for id in 0..6000 {assert!(hot.get(&format!("object-{id}"),([3;32],122)).is_some());}
        assert!(hot.get("object-1",([4;32],122)).is_none());
        let before=hot.state.lock().unwrap().bytes;let trimmed=hot.trim(64*1024);assert!(trimmed.bytes<=64*1024&&trimmed.bytes<before);
        assert!(hot.get("object-5999",([3;32],122)).is_some());
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
