//! Bounded in-memory encoded rasters. Snapshot/driver/format/quality/geometry
//! are part of the key; a hit avoids source IO, decoding and re-encoding.
use std::{collections::{BTreeMap, HashMap}, sync::{Arc, Mutex}, time::Instant};
use crate::{decoder::DecodedTile, engine::TileDecodeRequest};
mod disk;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    source: String, path: std::path::PathBuf, driver: String, format: &'static str,
    webp_method: u8, tiles: Vec<(u32, u32)>, edge: u32, reduction: u8, layers: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_source_does_not_reuse_pre_fix_pixels_after_remote_handle_is_consumed() {
        let cache=RasterCache {disk:None,limit:128,state:Mutex::new(State::default()),flights:Mutex::new(HashMap::new())};
        let original=request();cache.put("jp2grok",&original,&[tile(7)]);
        let mut prepared=original.clone();
        prepared.source.prepared=Some(Arc::new(crate::decoder::PreparedIo {file_ranges:false,view_reduction:0,
            pcrl_lowest_revision:true,technical_origin:None,predecoded_origin:(0,0),predecoded:None,overlapped_decode_ms:0.0,
            original_path:original.source.path.clone(),original_tiles:original.tiles.clone(),tile_offset_x:0,tile_offset_y:0,
            route:"s3-multipart-lowest-rpcl-ranges",stats:Default::default(),elapsed_ms:0.0,_file:None}));
        assert!(prepared.source.remote.is_none());assert!(cache.get("jp2grok",&prepared).is_none());
        cache.put("jp2grok",&prepared,&[tile(9)]);
        assert_eq!(cache.get("jp2grok",&prepared).unwrap()[0].raster.bytes[0],9);
        assert_eq!(cache.get("jp2grok",&original).unwrap()[0].raster.bytes[0],7);
    }
    #[test]
    fn declared_full_bounds_do_not_alias_a_smaller_decoded_raster() {
        let cache=RasterCache {disk:None,limit:128,state:Mutex::new(State::default()),flights:Mutex::new(HashMap::new())};
        let crop=request();cache.put("openjpeg",&crop,&[tile(7)]);
        let mut black=crop.clone();black.edge_mode=crate::edges::Mode::Black;
        assert!(cache.get("openjpeg",&black).is_none());
    }
    #[test]
    fn full_tiles_share_payload_and_inflight_work_across_edge_modes() {
        let cache=RasterCache {disk:None,limit:128,state:Mutex::new(State::default()),flights:Mutex::new(HashMap::new())};
        let crop=request();let mut black=crop.clone();black.edge_mode=crate::edges::Mode::Black;
        let mut transparent=crop.clone();transparent.edge_mode=crate::edges::Mode::Transparent;
        let flight=cache.flight("openjpeg",&crop).unwrap();
        assert!(Arc::ptr_eq(&flight,&cache.flight("openjpeg",&black).unwrap()));
        assert!(Arc::ptr_eq(&flight,&cache.flight("openjpeg",&transparent).unwrap()));
        let mut encoded=tile(7);encoded.raster.width=512;encoded.raster.height=512;cache.put("openjpeg",&crop,&[encoded.clone()]);
        for request in [&black,&transparent] {
            let hit=cache.get("openjpeg",request).unwrap();assert_eq!(hit[0].raster.bytes.as_ptr(),encoded.raster.bytes.as_ptr());
        }
        assert_eq!(cache.state.lock().unwrap().entries.len(),1);assert_eq!(cache.state.lock().unwrap().bytes,16);
    }
    #[test]
    fn mixed_batches_share_interiors_but_keep_three_distinct_edge_payloads() {
        let cache=RasterCache {disk:None,limit:1024,state:Mutex::new(State::default()),flights:Mutex::new(HashMap::new())};
        let mut request=request();request.source.width=700;request.source.height=530;request.tiles=vec![(0,0),(1,0),(0,1),(1,1)];
        let tiles=request.tiles.iter().map(|&(x,y)|{let mut t=tile(7);t.tile_x=x;t.tile_y=y;t.raster.width=if x==0 {512}else{188};t.raster.height=if y==0 {512}else{18};t}).collect::<Vec<_>>();
        cache.put("openjpeg",&request,&tiles);
        for mode in [crate::edges::Mode::Black,crate::edges::Mode::Transparent] {
            request.edge_mode=mode;assert!(cache.get("openjpeg",&request).is_none());
            let mut single=request.clone();single.tiles=vec![(0,0)];assert!(cache.get("openjpeg",&single).is_some());
            single.tiles=vec![(1,1)];assert!(cache.get("openjpeg",&single).is_none());
            cache.put("openjpeg",&request,&tiles[1..]);
        }
        assert_eq!(cache.state.lock().unwrap().entries.len(),10);assert_eq!(cache.state.lock().unwrap().bytes,160);
    }
    #[test]
    fn cropped_gpu_subsets_classify_restored_positions_using_local_bounds() {
        for (width,height,full) in [(512,512,true),(188,18,false)] {
            let cache=RasterCache {disk:None,limit:128,state:Mutex::new(State::default()),flights:Mutex::new(HashMap::new())};
            let mut original=request();original.tiles=vec![(3,4)];original.source.width=3*512+width;original.source.height=4*512+height;
            let mut prepared=original.clone();prepared.edge_mode=crate::edges::Mode::Black;prepared.tiles=vec![(0,0)];
            prepared.source.width=width;prepared.source.height=height;
            prepared.source.prepared=Some(Arc::new(crate::decoder::PreparedIo {file_ranges:false,view_reduction:0,pcrl_lowest_revision:false,technical_origin:None,predecoded_origin:(0,0),predecoded:None,overlapped_decode_ms:0.0,original_path:original.source.path.clone(),original_tiles:original.tiles.clone(),tile_offset_x:3,tile_offset_y:4,route:"fixture",stats:Default::default(),elapsed_ms:0.0,_file:None}));
            let mut encoded=tile(8);encoded.tile_x=3;encoded.tile_y=4;encoded.raster.width=width;encoded.raster.height=height;
            cache.put("openjpeg",&original,&[encoded]);
            assert_eq!(cache.get("openjpeg",&prepared).is_some(),full);
        }
    }
    #[test]
    fn tiles_are_reused_across_batch_shapes_without_duplicate_payload_admission() {
        let cache = RasterCache { disk: None, limit: 64, state: Mutex::new(State::default()), flights: Mutex::new(HashMap::new()) };
        let mut batch = request(); batch.tiles = vec![(0,0),(1,0)];
        let first = tile(7); let mut second = tile(8); second.tile_x = 1;
        cache.put("openjpeg",&batch,&[first.clone(),second.clone()]);
        let mut individual = batch.clone(); individual.tiles = vec![(1,0)];
        let hit = cache.get("openjpeg",&individual).unwrap();
        assert_eq!(hit.len(),1); assert_eq!(hit[0].raster.bytes,vec![8;16]);
        assert_eq!(hit[0].raster.bytes.as_ptr(),second.raster.bytes.as_ptr());
        cache.put("openjpeg",&individual,&[second]);
        assert_eq!(cache.state.lock().unwrap().bytes,32);
        assert!(cache.flight("openjpeg",&batch).is_none());
        individual.tiles = vec![(2,0)];
        assert!(cache.get("openjpeg",&individual).is_none());
        individual = batch.clone(); individual.layers = 2;
        assert!(cache.get("openjpeg",&individual).is_none());
    }
    fn request() -> TileDecodeRequest {
        TileDecodeRequest { output_geometry:None, edge_mode:crate::edges::Mode::Crop, capture_pixels:false, webp_method: crate::encoding_settings::snapshot(), source_id: "snapshot-one".into(), whole_view: false,
            source: crate::decoder::Jpeg2000Source::from_path("immutable.jp2", 1024, 1024),
            tiles: vec![(0, 0)], source_tile: 512, reduction: 0, layers: 1,
            format: river_core::RasterFormat::Png, decoder_threads: 1, max_native_tiles: 4, cached_tiles: vec![], work_ticket:None, cancellation: None }
    }
    fn tile(byte: u8) -> DecodedTile {
        DecodedTile { rgba:None, tile_x: 0, tile_y: 0, raster: crate::decoder::DecodedRaster {
            width: 1, height: 1, bytes: vec![byte; 16].into(), content_type: "image/png", decoder_route: "roi",
            decode_ms: 20.0, encode_ms: 5.0, source_cache_status: "s3-range-miss", source_cache_ms: 4.0,
            compressed_resolution_view: false, source_read_bytes: 100, source_range_requests: 2, source_cache_hit_bytes: 0 } }
    }
    #[test]
    fn never_reuses_another_snapshot_driver_quality_or_format() {
        let cache = RasterCache { disk: None, limit: 64, state: Mutex::new(State::default()), flights: Mutex::new(HashMap::new()) };
        let mut base = request();base.source.width=1;cache.put("openjpeg", &base, &[tile(7)]);
        assert_eq!(cache.get("openjpeg", &base).unwrap()[0].raster.bytes, vec![7; 16]);
        assert!(cache.get("grok", &base).is_none());
        let mut changed = base.clone(); changed.source_id = "snapshot-two".into();
        assert!(cache.get("openjpeg", &changed).is_none());
        changed = base.clone(); changed.layers = 2; assert!(cache.get("openjpeg", &changed).is_none());
        changed = base.clone(); changed.format = river_core::RasterFormat::Webp; assert!(cache.get("openjpeg", &changed).is_none());
        changed = base.clone(); changed.reduction = 1; assert!(cache.get("openjpeg", &changed).is_none());
        changed=base.clone();changed.edge_mode=crate::edges::Mode::Black;
        assert!(cache.get("openjpeg",&changed).is_none());cache.put("openjpeg",&changed,&[tile(8)]);
        assert_eq!(cache.get("openjpeg",&base).unwrap()[0].raster.bytes,vec![7;16]);
        changed.edge_mode=crate::edges::Mode::Transparent;assert!(cache.get("openjpeg",&changed).is_none());
    }
    #[test]
    fn eviction_and_hit_accounting_remain_bounded_and_honest() {
        let cache = RasterCache { disk: None, limit: 16, state: Mutex::new(State::default()), flights: Mutex::new(HashMap::new()) };
        let mut one = request();
        let encoded = tile(1);
        cache.put("openjpeg", &one, &[encoded.clone()]);
        let active = cache.get("openjpeg", &one).unwrap();
        let another = cache.get("openjpeg", &one).unwrap();
        assert_eq!(active[0].raster.bytes.as_ptr(), encoded.raster.bytes.as_ptr());
        assert_eq!(another[0].raster.bytes.as_ptr(), encoded.raster.bytes.as_ptr());
        one.source_id = "snapshot-two".into(); cache.put("openjpeg", &one, &[tile(2)]);
        assert!(cache.get("openjpeg", &request()).is_none());
        drop(encoded);
        assert_eq!(active[0].raster.bytes, vec![1;16]);
        let hit = cache.get("openjpeg", &one).unwrap();
        assert_eq!(hit[0].raster.source_read_bytes, 0); assert_eq!(hit[0].raster.decode_ms, 0.0);
        assert_eq!(cache.state.lock().unwrap().bytes, 16);
    }
    #[test]
    fn encoded_cache_never_retains_transient_capture_buffers() {
        let cache=RasterCache {disk:None,limit:64,state:Mutex::new(State::default()),flights:Mutex::new(HashMap::new())};
        let mut decoded=tile(1);decoded.rgba=Some(vec![9;4096].into());
        cache.put("openjpeg",&request(),&[decoded]);
        assert!(cache.get("openjpeg",&request()).unwrap()[0].rgba.is_none());
        assert_eq!(cache.state.lock().unwrap().bytes,16);
    }
}
impl Key {
    fn single(&self, position: (u32,u32)) -> Self {
        let mut key = self.clone(); key.tiles = vec![position]; key
    }
    fn new(driver: &str, request: &TileDecodeRequest) -> Self {
        let mut tiles = request.source.prepared.as_ref().map(|info| info.original_tiles.clone()).unwrap_or_else(|| request.tiles.clone()); tiles.sort_unstable();
        let driver=if request.edge_mode==crate::edges::Mode::Crop {driver.to_owned()} else {format!("{driver}:edge={}",request.edge_mode.as_str())};
        let driver=match request.output_geometry {Some(g)=>format!("{driver}:output={}x{}x{}",g.edge,g.width,g.height),None=>driver};
        // Older Grok responses for this class can contain incorrect pixels.
        // Isolate only the corrected class, preserving unrelated disk-cache hits.
        let driver=if (request.source.remote.as_ref().is_some_and(|r|r.header.progression==3 && r.header.layers==1 && !r.header.is_ht
            && request.reduction.saturating_add(1)==r.header.levels)
            && std::env::var("RIVER_S3_MULTIPART_PLT").is_ok_and(|v|v=="true" || v=="1"))
            || request.source.prepared.as_ref().is_some_and(|p|p.pcrl_lowest_revision) {
            format!("{driver}:pcrl-lowest-rpcl-v1")
        }else{driver};
        Self { source: request.source_id.clone(), path: request.source.prepared.as_ref().map(|info| info.original_path.clone()).unwrap_or_else(|| request.source.path.clone()), driver,
            webp_method: if request.format==river_core::RasterFormat::Webp {request.webp_method} else {255}, format: request.format.content_type(), tiles, edge: request.source_tile,
            reduction: request.reduction, layers: request.layers }
    }
}
// Only unambiguously full, integral-resolution tiles are identical across
// edge modes. Prepared GPU subsets use local dimensions and restored output
// positions, so remove their offset before checking the image bounds.
pub(crate) fn full_tile(request:&TileDecodeRequest,position:(u32,u32))->bool {
    if request.output_geometry.is_some() {return false;}
    let Some(scale)=1u32.checked_shl(u32::from(request.reduction)) else {return false;};
    if request.source_tile==0 || request.source_tile%scale!=0 || request.source.reference_origin!=(0,0) {return false;}
    let offset=request.source.prepared.as_ref().map(|info|(info.tile_offset_x,info.tile_offset_y)).unwrap_or((0,0));
    let (Some(x),Some(y))=(position.0.checked_sub(offset.0),position.1.checked_sub(offset.1)) else {return false;};
    (u64::from(x)+1)*u64::from(request.source_tile)<=u64::from(request.source.width)
        && (u64::from(y)+1)*u64::from(request.source_tile)<=u64::from(request.source.height)
}
struct Entry { tile: Arc<DecodedTile>, bytes: usize, touched: u64 }
#[derive(Default)]
struct State { entries: HashMap<Key, Entry>, lru: BTreeMap<u64, Key>, bytes: usize, clock: u64 }
pub struct RasterCache { disk: Option<disk::Disk>, limit: usize, state: Mutex<State>, flights: Mutex<HashMap<Key, std::sync::Weak<tokio::sync::Mutex<()>>>> }
impl RasterCache {
    pub fn from_env() -> Arc<Self> {
        Arc::new(Self { disk: disk::Disk::from_env(), limit: std::env::var("RIVER_RASTER_CACHE_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(64 * 1024 * 1024),
            state: Mutex::new(State::default()), flights: Mutex::new(HashMap::new()) })
    }
    pub fn flight(&self, driver: &str, request: &TileDecodeRequest) -> Option<Arc<tokio::sync::Mutex<()>>> {
        if self.limit == 0 { return None; }
        let mut key = Key::new(driver, request);
        if request.edge_mode!=crate::edges::Mode::Crop && key.tiles.iter().all(|&position|full_tile(request,position)) {key.driver=driver.to_owned();}
        {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let nominal=request.source_tile.div_ceil(1u32.checked_shl(u32::from(request.reduction)).unwrap_or(u32::MAX));
            if key.tiles.iter().all(|&position| state.entries.get(&key.single(position)).is_some_and(|entry|
                request.edge_mode==crate::edges::Mode::Crop || key.driver!=driver || (entry.tile.raster.width==nominal && entry.tile.raster.height==nominal))) { return None; }
        }
        let mut flights = self.flights.lock().unwrap_or_else(|error| error.into_inner());
        flights.retain(|_, gate| gate.strong_count() > 0);
        let gate = flights.get(&key).and_then(std::sync::Weak::upgrade).unwrap_or_else(|| {
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            flights.insert(key, Arc::downgrade(&gate)); gate
        });
        Some(gate)
    }
    pub fn get(&self, driver: &str, request: &TileDecodeRequest) -> Option<Vec<DecodedTile>> {
        if self.limit == 0 { return None; }
        let started = Instant::now(); let key = Key::new(driver, request);
        let keys: Vec<_> = key.tiles.iter().map(|&position| key.single(position)).collect();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut tiles = Vec::with_capacity(keys.len());
        let mut used_keys=Vec::with_capacity(keys.len());
        let nominal=request.source_tile.div_ceil(1u32.checked_shl(u32::from(request.reduction)).unwrap_or(u32::MAX));
        for mut key in keys {
            let mut disk_tile=None;
            if request.edge_mode!=crate::edges::Mode::Crop && full_tile(request,key.tiles[0]) && !state.entries.contains_key(&key) {
                let mut crop=key.clone();crop.driver=driver.to_owned();
                if state.entries.get(&crop).is_some_and(|entry|entry.tile.raster.width==nominal && entry.tile.raster.height==nominal) {key=crop;}
                else if let Some(tile)=self.disk.as_ref().and_then(|disk|disk.get(&crop)).filter(|tile|tile.raster.width==nominal && tile.raster.height==nominal) {
                    key=crop;disk_tile=Some(tile);
                }
            }
            used_keys.push(key.clone());
            if let Some(entry)=state.entries.get(&key) { tiles.push(entry.tile.as_ref().clone()); }
            else {
                let tile=match disk_tile {Some(tile)=>tile,None=>self.disk.as_ref()?.get(&key)?};
                let bytes=tile.raster.bytes.len();
                if bytes<=self.limit {
                    while state.bytes+bytes>self.limit || state.entries.len()>=4096 {
                        let Some((_,oldest))=state.lru.pop_first() else { break; };
                        if let Some(old)=state.entries.remove(&oldest) { state.bytes-=old.bytes; }
                    }
                    state.clock+=1;let touched=state.clock;
                    state.lru.insert(touched,key.clone());state.bytes+=bytes;
                    state.entries.insert(key.clone(),Entry { tile:Arc::new(tile.clone()),bytes,touched });
                }
                tiles.push(tile);
            }
        }
        for key in used_keys {
            let Some(entry)=state.entries.get(&key) else { continue; };
            let old = entry.touched;
            state.lru.remove(&old); state.clock += 1; let clock = state.clock;
            state.lru.insert(clock, key.clone()); state.entries.get_mut(&key).unwrap().touched = clock;
        }
        drop(state);
        for tile in &mut tiles {
            tile.raster.decoder_route = "encoded-raster-cache";
            tile.raster.decode_ms = 0.0; tile.raster.encode_ms = 0.0;
            tile.raster.source_cache_status = "encoded-raster-hit";
            tile.raster.source_cache_ms = crate::decoder::elapsed_ms(started);
            tile.raster.source_read_bytes = 0; tile.raster.source_range_requests = 0; tile.raster.source_cache_hit_bytes = 0;
        }
        Some(tiles)
    }
    pub fn put(&self, driver: &str, request: &TileDecodeRequest, tiles: &[DecodedTile]) {
        if tiles.is_empty() || self.limit == 0 { return; }
        let base = Key::new(driver, request);
        if let Some(disk)=&self.disk { for tile in tiles { disk.put(&base.single((tile.tile_x,tile.tile_y)),tile); } }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for tile in tiles {
        let bytes = tile.raster.bytes.len();
        if bytes > self.limit { continue; }
        let key = base.single((tile.tile_x,tile.tile_y));
        if let Some(old) = state.entries.remove(&key) { state.bytes -= old.bytes; state.lru.remove(&old.touched); }
        while state.bytes + bytes > self.limit || state.entries.len() >= 4096 {
            let Some((_, oldest)) = state.lru.pop_first() else { break; };
            if let Some(entry) = state.entries.remove(&oldest) { state.bytes -= entry.bytes; }
        }
        state.clock += 1; let touched = state.clock;
        state.lru.insert(touched, key.clone()); state.bytes += bytes;
        let mut stored=tile.clone();stored.rgba=None;
        state.entries.insert(key, Entry { tile: Arc::new(stored), bytes, touched });
        }
    }
}

#[cfg(test)]
mod output_key_tests {
    use super::*;
    #[test]
    fn explicit_output_geometry_is_in_cache_and_singleflight_identity() {
        let request=TileDecodeRequest {output_geometry:None,edge_mode:crate::edges::Mode::Crop,source_id:"same".into(),webp_method:0,capture_pixels:false,whole_view:false,source:crate::decoder::Jpeg2000Source::from_path("same.jp2",1024,1024),tiles:vec![(0,0)],cached_tiles:vec![],work_ticket:None,source_tile:512,reduction:0,layers:0,format:river_core::RasterFormat::Png,decoder_threads:1,max_native_tiles:4,cancellation:None};
        let old=Key::new("driver",&request);let mut explicit=request.clone();
        explicit.output_geometry=Some(crate::edges::OutputGeometry {edge:256,width:256,height:256});
        let one=Key::new("driver",&explicit);assert_ne!(old,one);
        explicit.output_geometry.as_mut().unwrap().edge=512;assert_ne!(one,Key::new("driver",&explicit));
        explicit.output_geometry.as_mut().unwrap().width=128;assert_ne!(one,Key::new("driver",&explicit));
        assert!(!full_tile(&explicit,(0,0)));
    }
}
