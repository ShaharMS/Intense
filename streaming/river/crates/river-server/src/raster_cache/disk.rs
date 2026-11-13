//! Disposable, checksummed encoded tiles; versioned identities prevent stale reuse.
use super::{DecodedTile, Key};
use sha2::{Digest, Sha256};
use std::{io::{Read,Write}, path::PathBuf, sync::Mutex};
const MAX_TILE: usize = 8*1024*1024;
const HEADER: usize = 49;
pub(super) struct Disk { root: PathBuf, budget: u64, state: Mutex<crate::cache_inventory::Inventory> }
impl Disk {
    pub fn from_env() -> Option<Self> {
        if !crate::storage_mode::disk_cache_enabled() { return None; }
        let budget=crate::resources::plan().caches["raster_disk"].effective_target_bytes as u64;
        if budget==0 { return None; }
        let root = std::env::var_os("RIVER_RASTER_DISK_CACHE_ROOT").map(PathBuf::from)
            .or_else(|| std::env::var_os("RIVER_S3_CACHE_ROOT").map(|p|PathBuf::from(p).join("encoded-tiles-v1")))?;
        Self::new(root,budget)
    }
    fn new(root:PathBuf,budget:u64) -> Option<Self> {
        std::fs::create_dir_all(&root).ok()?;
        let entries=crate::cache_inventory::Inventory::open(root.clone(),budget,crate::resources::entry_limit("RIVER_RASTER_DISK_CACHE_ENTRIES",budget as usize),|path|path.file_name().and_then(|n|n.to_str()).is_some_and(|n|n.len()==64&&n.bytes().all(|b|b.is_ascii_hexdigit()))).ok()?;
        Some(Self{root,budget,state:Mutex::new(entries)})
    }
    pub fn snapshot(&self)->crate::resources::Occupancy{let s=self.state.lock().unwrap_or_else(|e|e.into_inner());crate::resources::Occupancy{bytes:s.bytes as usize,entries:s.entries.len(),persistence_location:Some(self.root.to_string_lossy().into_owned()),..Default::default()}}
    pub(super) fn id(key: &Key) -> String {
        // Bump schema when pixel conversion/codec defaults change. Include encoder settings.
        hex::encode(Sha256::digest(format!("river-encoded-v2:{key:?}").as_bytes()))
    }
    pub fn get(&self,key:&Key) -> Option<DecodedTile> {
        if crate::resources::budget("raster_disk") == 0 { return None; }
        let id=Self::id(key);
        let path=self.root.join(&id);
        let file=match std::fs::File::open(path){Ok(f)=>f,Err(_)=>{crate::resources::miss("raster_disk");return None;}};
        let mut bytes=Vec::new(); file.take((MAX_TILE+HEADER+1) as u64).read_to_end(&mut bytes).ok()?;
        let tile = Self::decode(key, &bytes)?;
        self.state.lock().unwrap_or_else(|e|e.into_inner()).touch(&id);
        crate::resources::hit("raster_disk", tile.raster.bytes.len());
        Some(tile)
    }
    pub(super) fn decode(key: &Key, bytes: &[u8]) -> Option<DecodedTile> {
        if bytes.len()<HEADER || bytes.len()>MAX_TILE+HEADER || &bytes[..8]!=b"RIVRC001" { return None; }
        let mut digest=Sha256::new();digest.update(&bytes[..17]);digest.update(&bytes[HEADER..]);
        if digest.finalize().as_slice()!=&bytes[17..49] { return None; }
        let width=u32::from_le_bytes(bytes[8..12].try_into().ok()?);
        let height=u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        if width==0 || height==0 || width>32768 || height>32768 { return None; }
        Some(DecodedTile { rgba:None, tile_x:key.tiles[0].0,tile_y:key.tiles[0].1,raster:crate::decoder::DecodedRaster {
            width,height,bytes:bytes[HEADER..].to_vec().into(),content_type:key.format,
            decoder_backend: "encoded-raster-cache", decoder_route:"encoded-raster-disk-cache",decode_ms:0.0,encode_ms:0.0,
            source_cache_status:"encoded-raster-disk-hit",source_cache_ms:0.0,
            compressed_resolution_view:bytes[16]!=0,source_read_bytes:0,source_range_requests:0,source_cache_hit_bytes:0,
        }})
    }
    pub(super) fn encode(tile: &DecodedTile) -> Vec<u8> {
        let mut bytes = b"RIVRC001".to_vec();
        bytes.extend(tile.raster.width.to_le_bytes());
        bytes.extend(tile.raster.height.to_le_bytes());
        bytes.push(u8::from(tile.raster.compressed_resolution_view));
        let mut digest = Sha256::new();
        digest.update(&bytes);
        digest.update(&tile.raster.bytes);
        bytes.extend(digest.finalize());
        bytes.extend(&tile.raster.bytes);
        bytes
    }
    pub fn put(&self,key:&Key,tile:&DecodedTile) {
        if crate::resources::budget("raster_disk") == 0 { return; }
        let data=&tile.raster.bytes;
        let size=(data.len()+HEADER) as u64;
        if data.len()>MAX_TILE || size>self.budget { return; }
        let id=Self::id(key);
        if self.get(key).is_some() { return; }
        let mut state=self.state.lock().unwrap_or_else(|e|e.into_inner());
        let removed=match state.make_room(&id,size){Ok(n)=>n,Err(_)=>return};crate::resources::evict("raster_disk",removed);
        let old=state.entries.get(&id).map_or(0,|v|v.0);if state.bytes.saturating_sub(old).saturating_add(size)>self.budget{return;}
        let result=(|| -> std::io::Result<()> {
            let mut file=tempfile::NamedTempFile::new_in(&self.root)?;
            let mut header=b"RIVRC001".to_vec();header.extend(tile.raster.width.to_le_bytes());header.extend(tile.raster.height.to_le_bytes());
            header.push(u8::from(tile.raster.compressed_resolution_view));
            let mut digest=Sha256::new();digest.update(&header);digest.update(data);
            file.write_all(&header)?;file.write_all(&digest.finalize())?;file.write_all(data)?;
            file.persist(self.root.join(&id)).map_err(|e|e.error)?;Ok(())
        })();
        if result.is_ok() { state.record(id,size); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(source:&str)->Key { Key { webp_method:255, source:source.into(),path:"same.jp2".into(),driver:"grok".into(),format:"image/png",tiles:vec![(2,3)],edge:512,reduction:2,layers:1 } }
    fn tile()->DecodedTile { DecodedTile { rgba:None, tile_x:2,tile_y:3,raster:crate::decoder::DecodedRaster {
        width:16,height:16,bytes:vec![7;100].into(),content_type:"image/png",decoder_backend: "encoded-raster-cache", decoder_route:"roi",decode_ms:1.0,encode_ms:1.0,
        source_cache_status:"miss",source_cache_ms:1.0,compressed_resolution_view:true,source_read_bytes:12,source_range_requests:1,source_cache_hit_bytes:0,
    } } }
    #[test]
    fn restart_reuse_is_snapshot_specific_and_corruption_is_repaired() {
        let root=tempfile::tempdir().unwrap();let k=key("version1");
        let disk=Disk::new(root.path().into(),1024).unwrap();disk.put(&k,&tile());drop(disk);
        let disk=Disk::new(root.path().into(),1024).unwrap();
        let hit=disk.get(&k).unwrap();assert_eq!(hit.raster.bytes,vec![7;100]);assert_eq!(hit.raster.source_read_bytes,0);
        assert!(disk.get(&key("version2")).is_none());
        let mut changed=k.clone();changed.layers=2;assert!(disk.get(&changed).is_none());
        let path=root.path().join(Disk::id(&k));let mut bytes=std::fs::read(&path).unwrap();bytes[8]^=1;
        std::fs::write(path,bytes).unwrap();assert!(disk.get(&k).is_none());
        disk.put(&k,&tile());assert!(disk.get(&k).is_some());
    }
    #[test]
    fn concurrent_admission_and_restart_obey_disk_budget() {
        let root=tempfile::tempdir().unwrap();let disk=std::sync::Arc::new(Disk::new(root.path().into(),300).unwrap());
        let threads=(0..16).map(|n| { let disk=disk.clone();std::thread::spawn(move||disk.put(&key(&n.to_string()),&tile())) }).collect::<Vec<_>>();
        for thread in threads { thread.join().unwrap(); }
        let bytes:u64=std::fs::read_dir(root.path()).unwrap().flatten().map(|e|e.metadata().unwrap().len()).sum();assert!(bytes<=300);
        drop(disk);let disk=Disk::new(root.path().into(),150).unwrap();
        assert!(disk.state.lock().unwrap().entries.values().map(|v|v.0).sum::<u64>()<=150);
    }
}
