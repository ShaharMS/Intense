//! Demand-driven, version-pinned JPEG 2000 reads. Only compressed source bytes
//! are cached. A decoder receives a temporary, valid packet subset, never a
//! zero-filled sparse copy of the object or a precomputed raster pyramid.
use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, io::Write, ops::Range, sync::{Arc, Mutex}, time::{Duration, Instant}};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{StreamExt, TryStreamExt};
use object_store::{GetOptions, GetRange, ObjectMeta, ObjectStore, ObjectStoreExt, path::Path};
use tokio::sync::watch;
use tracing::Instrument;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::decoder::NativeTileLayoutHint;
mod index_cache;
mod jpip_index;
mod index_shards;
mod recent_reads;
mod tile_header_cache;
mod packet_plan_cache;
pub(crate) mod read_trace;
pub(crate) mod wire_budget;
mod precinct_roi;
mod multipart;
mod multipart_prefix;
mod layered_packets;
mod structure;
mod single_prefix;
mod index_only_prefix;
mod streamed_header;
pub(crate) const HEADER_WINDOW_MAX_BYTES:u64=8*1024*1024;
pub(crate) const HEADER_WINDOW_RETAINED_BYTES:usize=streamed_header::RETAINED_LIMIT;
mod single_roi;
mod resolution_view;
mod payload_live;
#[cfg(test)] mod seekable_probe;
#[cfg(all(feature="grok",unix))] mod pipeline;
#[cfg(all(feature="grok",unix))] mod curl_transport;

pub(crate) fn validate_serial_transport()->Result<()> {
    let mode=std::env::var("RIVER_S3_SERIAL_TRANSPORT").unwrap_or_else(|_|"sdk".into());
    ensure!(mode=="sdk" || mode=="curl" || mode=="auto","RIVER_S3_SERIAL_TRANSPORT must be sdk, curl, or auto");
    validate_transport_kind(crate::read_policy::settings().effective_serial_transport())
}
pub(crate) fn validate_transport_kind(kind:crate::read_policy::SerialTransport)->Result<()> {
    if kind==crate::read_policy::SerialTransport::Curl {
        #[cfg(all(feature="grok",unix))] curl_transport::initialize()?;
        #[cfg(not(all(feature="grok",unix)))] bail!("native S3 curl transport is not compiled for this platform");
    }
    Ok(())
}
#[cfg(any(all(feature="grok",unix),test))]
async fn retry_delay(delay:Duration,mut receiver:Option<watch::Receiver<bool>>)->Result<()> {
    let stopped=async {if let Some(r)=&mut receiver {if !*r.borrow(){let _=r.changed().await;}}else{std::future::pending::<()>().await;}};
    tokio::select! {_=tokio::time::sleep(delay)=>Ok(()),_=stopped=>bail!("S3 request superseded")}
}
#[cfg(test)] mod retry_delay_tests {
    use super::*;
    #[tokio::test] async fn supersession_interrupts_retry_backoff() {
        let (sender,receiver)=watch::channel(false);
        let waiting=tokio::spawn(retry_delay(Duration::from_secs(5),Some(receiver)));
        tokio::time::sleep(Duration::from_millis(10)).await;sender.send_replace(true);
        let error=tokio::time::timeout(Duration::from_millis(250),waiting).await.unwrap().unwrap().unwrap_err();
        assert!(error.to_string().contains("superseded"));
    }
    #[tokio::test] async fn closed_cancellation_channel_does_not_wait_for_backoff() {
        let (sender,receiver)=watch::channel(false);drop(sender);
        assert!(tokio::time::timeout(Duration::from_millis(250),retry_delay(Duration::from_secs(5),Some(receiver))).await.unwrap().is_err());
    }
}

const BLOCK: u64 = 1024;
const ADAPTIVE_PREFIX_RANGES:usize=usize::MAX-1;
pub(crate) const PREFIX_MERGE_BYTES:u64=8*1024*1024;
fn retain_requested_blocks(leases:&mut HashMap<u64,Arc<[u8]>>,spans:&[(bool,Range<u64>)],block_size:u64) {
    let wanted=spans.iter().filter(|(keep,_)|*keep).flat_map(|(_,r)|
        (r.start/block_size..r.end.div_ceil(block_size)).map(|block|block*block_size)).collect::<HashSet<_>>();
    leases.retain(|offset,_|wanted.contains(offset));
}
const INITIAL_PROBE:u64=3*BLOCK;
const MAX_HEADER: usize = 1024 * 1024;
const MAX_HEADER_ENTRIES: usize = 4096;
const MAX_PACKETS: usize = 1_000_000;
pub(crate) const INDEX_CALIBRATION_READS:u64=8;

#[derive(Default)]
struct CompatibilityCache {
    files: HashMap<String, (Arc<tempfile::NamedTempFile>, u64, u64)>,
    bytes: u64,
    clock: u64,
}
impl CompatibilityCache {
    fn shared() -> &'static Mutex<Self> {
        static CACHE: std::sync::OnceLock<Mutex<CompatibilityCache>> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(Self::default()))
    }
    fn get(&mut self, id: &str) -> Option<Arc<tempfile::NamedTempFile>> {
        self.clock += 1;
        let entry = self.files.get_mut(id)?;
        entry.2 = self.clock;
        Some(entry.0.clone())
    }
    fn put(&mut self, id: String, file: Arc<tempfile::NamedTempFile>, bytes: u64, limit: u64) {
        if bytes > limit || limit == 0 { return; }
        if let Some(old) = self.files.remove(&id) { self.bytes -= old.1; }
        while self.bytes + bytes > limit || self.files.len() >= 256 {
            let Some(id) = self.files.iter().min_by_key(|(_, entry)| entry.2).map(|(id, _)| id.clone()) else { break; };
            let old = self.files.remove(&id).unwrap();
            self.bytes -= old.1;
        }
        self.clock += 1;
        self.bytes += bytes;
        self.files.insert(id, (file, bytes, self.clock));
    }
}

#[derive(Debug, Default, Clone)]
pub struct ReadStats { pub bytes: u64, pub requests: u64, pub cache_bytes: u64 }

/// A bounded GET selects one object version and supplies its first reader block.
pub struct InitialRead {pub meta:ObjectMeta,seed:HeaderSeed}
struct HeaderSeed {prefix:Option<Vec<u8>>,stats:ReadStats,elapsed_us:u64,budget:Arc<wire_budget::Budget>}
impl InitialRead {
    pub async fn fetch(store:&dyn ObjectStore,location:&Path)->Result<Self> {
        use sha2::{Digest,Sha256};
        let probe_bytes=INITIAL_PROBE.min(env_limit("RIVER_S3_REQUEST_BYTES",64*1024*1024).max(1) as u64);
        let trace_object=format!("probe-{}",hex::encode(Sha256::digest(location.as_ref().as_bytes())));
        let mut trace=read_trace::Read::begin(read_trace::context_id(),&trace_object,std::panic::Location::caller(),"initialHeader",0..probe_bytes,BLOCK,None,None,"sdk");
        let started=Instant::now();
        let budget=wire_budget::Budget::new(env_limit("RIVER_S3_REQUEST_RANGES",1024).max(1) as u64);
        let result=budget.scope(trace.scope(store.get_opts(location,GetOptions {range:Some(GetRange::Bounded(0..probe_bytes)),..Default::default()}))).await?;
        let mut meta=result.meta.clone();let end=meta.size.min(probe_bytes);
        ensure!(result.range==(0..end),"S3 endpoint ignored or changed the bounded initial header range");
        let headers_elapsed=trace.is_recording().then(||started.elapsed());
        let bytes=bounded_body(result,end).await?;
        if let Some(headers)=headers_elapsed {trace.transfer_timing(headers,started.elapsed());}
        trace.finish(bytes.len() as u64,true,None);
        let elapsed_us=started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let stats=ReadStats {bytes:bytes.len() as u64,requests:1,cache_bytes:0};
        let prefix=if meta.e_tag.is_some() || meta.version.is_some() {Some(bytes)} else {
            // Some compatible stores expose their version only on HEAD. Do not
            // mix the unbound prefix with that later snapshot: reread it pinned.
            meta=budget.scope(store.head(location)).await?;None
        };
        Ok(Self {meta,seed:HeaderSeed {prefix,stats,elapsed_us,budget}})
    }
}

#[derive(Clone)]
pub struct PreparedSubset {
    pub view_reduction:u8,
    pub technical_origin:Option<(u32,u32)>,
    pub bytes: Vec<u8>, pub route: &'static str, jp2: bool,
    pub width: u32, pub height: u32, pub layout: NativeTileLayoutHint,
    pub offset_x: u32, pub offset_y: u32,
}
impl PreparedSubset {
    pub fn materialize(&self) -> Result<tempfile::NamedTempFile> {
        let root = std::env::var_os("RIVER_S3_CACHE_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("river-s3-cache")).join("range-views");
        std::fs::create_dir_all(&root)?;
        let mut file = tempfile::Builder::new().prefix("river-range-").suffix(if self.jp2 { ".jp2" } else { ".j2k" }).tempfile_in(root)?;
        file.write_all(&self.bytes)?; file.flush()?;
        Ok(file)
    }
}

#[derive(Debug)]
struct CachedBlock { data: Arc<[u8]>, touched: u64 }
#[derive(Debug, Default)]
struct CacheState { blocks: HashMap<(Arc<str>, u64, u64), CachedBlock>, touched: BTreeMap<u64, (Arc<str>, u64, u64)>, bytes: usize, clock: u64 }
#[derive(Debug)]
pub struct RangeCache { limit: usize, state: Mutex<CacheState> }
impl RangeCache {
    pub fn from_env() -> Arc<Self> {
        Arc::new(Self { limit: std::env::var("RIVER_S3_RANGE_CACHE_BYTES").ok().and_then(|value|value.parse().ok()).unwrap_or(64 * 1024 * 1024), state: Mutex::new(CacheState::default()) })
    }
    fn get(&self, id: &Arc<str>, block_size: u64, offset: u64) -> Option<Arc<[u8]>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.clock += 1;
        let clock = state.clock;
        let key = (id.clone(), block_size, offset);
        let old = state.blocks.get(&key)?.touched;
        state.touched.remove(&old);
        state.touched.insert(clock, key.clone());
        let block = state.blocks.get_mut(&key).unwrap();
        block.touched = clock;
        Some(block.data.clone())
    }
    /// Reuse only cached edges of a compatibility chunk. A missing interior
    /// remains one GET, so fragmentation never increases storage request count.
    fn compatibility_edges(&self, id: &Arc<str>, block_size: u64, range: Range<u64>) -> (Vec<u8>, Range<u64>, Vec<u8>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut start = range.start;
        let mut end = range.end;
        let mut prefix = Vec::new();
        while start < end {
            let offset = start / block_size * block_size;
            let Some(block) = state.blocks.get(&(id.clone(), block_size, offset)) else { break; };
            let within = (start-offset) as usize;
            if within >= block.data.len() { break; }
            let count = (end-start).min((block.data.len()-within) as u64) as usize;
            prefix.extend_from_slice(&block.data[within..within+count]);
            start += count as u64;
        }
        let mut tails = Vec::new();
        while end > start {
            let offset = (end-1) / block_size * block_size;
            let Some(block) = state.blocks.get(&(id.clone(), block_size, offset)) else { break; };
            let within = (end-offset) as usize;
            if within > block.data.len() { break; }
            let begin = start.max(offset);
            tails.push(block.data[(begin-offset) as usize..within].to_vec());
            end = begin;
        }
        let suffix = tails.into_iter().rev().flatten().collect();
        (prefix, start..end, suffix)
    }
    fn put(&self, id: &Arc<str>, block_size: u64, offset: u64, data: &[u8]) -> Vec<(u64, Arc<[u8]>)> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut leases = vec![];
        for (index, chunk) in data.chunks(block_size as usize).enumerate() {
            let position = offset + index as u64 * block_size;
            let bytes: Arc<[u8]> = Arc::from(chunk);
            leases.push((position, bytes.clone()));
            if chunk.len() > self.limit { continue; }
            let key = (id.clone(), block_size, position);
            if let Some(old) = state.blocks.remove(&key) { state.bytes -= old.data.len(); state.touched.remove(&old.touched); }
            while state.bytes + chunk.len() > self.limit {
                let Some((_, key)) = state.touched.pop_first() else { break; };
                let old = state.blocks.remove(&key).unwrap();
                state.bytes -= old.data.len();
            }
            state.clock += 1;
            let touched = state.clock;
            state.touched.insert(touched, key.clone());
            state.bytes += chunk.len();
            state.blocks.insert(key, CachedBlock { data: bytes, touched });
        }
        leases
    }
}
fn env_limit(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).filter(|&n| n > 0).unwrap_or(default)
}

/// Dropping the HTTP/WebSocket request cancels pending network reads even when
/// its decoder is already running in spawn_blocking.
pub struct CancelOnDrop(watch::Sender<bool>);
impl CancelOnDrop {
    pub fn new() -> (Self, watch::Receiver<bool>) { let (tx, rx) = watch::channel(false); (Self(tx), rx) }
}
impl Drop for CancelOnDrop { fn drop(&mut self) { let _ = self.0.send(true); } }

type ParsedTileHeader=(Vec<u8>,u64,Option<Arc<Vec<u64>>>,bool);
#[derive(Clone,Copy)]
struct IndexRangeAllowance {base:u64,before:u64}
#[derive(Default)]
struct AutoSerial {misses:u32,disabled:bool,ready:bool}
impl AutoSerial {
    fn allow(&mut self,serial_index:bool,tiles:u64,latency_us:u64,active:usize)->bool {
        self.misses=self.misses.saturating_add(1);
        // Once contended, do not switch late and open another connection for
        // the last few reads. Requests which begin isolated can also fall back.
        if active>1 {self.disabled=true;}
        !self.disabled && self.misses>16 && serial_index && tiles>=128 && latency_us<2000
    }
}
#[cfg(test)] mod auto_serial_tests {
    use super::*;
    #[test] fn sustained_isolated_discovery_can_switch_after_required_reads() {
        let mut choice=AutoSerial::default();for _ in 0..16 {assert!(!choice.allow(true,451,300,1));}assert!(choice.allow(true,451,300,1));
    }
    #[test] fn contention_disables_switching_even_after_other_work_finishes() {
        let mut choice=AutoSerial::default();for _ in 0..20 {assert!(!choice.allow(true,451,300,8));}assert!(!choice.allow(true,451,300,1));
        let mut isolated=AutoSerial::default();for _ in 0..17 {isolated.allow(true,451,300,1);}assert!(!isolated.allow(true,451,300,2));assert!(!isolated.allow(true,451,300,1));
    }
    #[test] fn small_indexed_and_high_latency_sources_keep_sdk() {
        for (serial,tiles,latency) in [(true,1,300),(false,451,300),(true,451,3000)] {let mut choice=AutoSerial::default();for _ in 0..40 {assert!(!choice.allow(serial,tiles,latency,1));}}
    }
}
pub struct ReadContext {
    trace_id:u64,
    trace_index:Option<usize>,
    #[cfg(all(feature="grok",unix))] curl_lease:Option<curl_transport::Lease>,
    curl_requests:u64,
    serial_transport:crate::read_policy::SerialTransport,
    auto_serial:AutoSerial,
    cache_only:bool,
    #[cfg(all(feature="grok",unix))] pipeline_requested:bool,
    #[cfg(all(feature="grok",unix))] pipeline:Option<pipeline::Pipeline>,
    validated:HashMap<u64,TilePart>,
    tile_headers:HashMap<u64,(TilePart,ParsedTileHeader)>,
    orders:HashMap<(u16,Option<u8>),packet_plan_cache::Plan>,
    packet_lengths:HashMap<u64,(usize,u64)>,
    metadata_bytes:usize,
    metadata_limit:usize,
    pub stats: ReadStats,
    limit: u64,
    requests_limit: u64,
    wire_budget:Arc<wire_budget::Budget>,
    explicit_request_limit: bool,
    main_header: bool,
    reported_missing_plt: bool,
    #[cfg(test)]
    legacy_multipart_plan: bool,
    deadline: Instant,
    cancel: Option<watch::Receiver<bool>>,
    // At most 8 MiB of required object/packet blocks, leased during preparation.
    pinned: HashMap<u64, Arc<[u8]>>,
    recent: recent_reads::RecentReads,
}
impl ReadContext {
    pub fn new(cancel: Option<watch::Receiver<bool>>) -> Self {
        let configured_ranges=std::env::var("RIVER_S3_REQUEST_RANGES").ok().and_then(|v|v.parse::<u64>().ok()).filter(|&n|n>0);
        Self { trace_id:read_trace::context_id(),trace_index:None,cache_only:false,curl_requests:0,serial_transport:crate::read_policy::settings().effective_serial_transport(),auto_serial:AutoSerial::default(),
            #[cfg(test)] legacy_multipart_plan:false,
            #[cfg(all(feature="grok",unix))] curl_lease:None,
            #[cfg(all(feature="grok",unix))] pipeline:None,
            #[cfg(all(feature="grok",unix))] pipeline_requested:false,
            validated:HashMap::new(),tile_headers:HashMap::new(),orders:HashMap::new(),packet_lengths:HashMap::new(),metadata_bytes:0,
            metadata_limit:std::env::var("RIVER_S3_PREP_METADATA_BYTES").ok().and_then(|v|v.parse().ok()).unwrap_or(4*1024*1024),
            stats: ReadStats::default(), limit: env_limit("RIVER_S3_REQUEST_BYTES", 64 * 1024 * 1024) as u64,
            wire_budget:wire_budget::Budget::new(configured_ranges.unwrap_or(1024)), requests_limit: configured_ranges.unwrap_or(1024), explicit_request_limit: configured_ranges.is_some(),
            main_header:false,reported_missing_plt:false,deadline: Instant::now() + Duration::from_secs(30), cancel, pinned: HashMap::new(), recent: recent_reads::RecentReads::default() }
    }
    // Credit only bounded index-phase requests. Payload requests retain the
    // allowance left before indexing, and explicit administrator caps win.
    fn begin_index_ranges(&mut self,native:u64)->Option<IndexRangeAllowance> {
        let limit=native.saturating_add(256).min(16_384);
        if self.explicit_request_limit || native<=self.requests_limit || limit<=self.requests_limit {return None;}
        let saved=IndexRangeAllowance {base:self.requests_limit,before:self.stats.requests};
        self.requests_limit=limit;Some(saved)
    }
    fn finish_index_ranges(&mut self,saved:Option<IndexRangeAllowance>)->u64 {
        if let Some(saved)=saved {
            let credited=self.stats.requests.saturating_sub(saved.before);
            self.requests_limit=saved.base.saturating_add(credited);credited
        } else {0}
    }
    fn allow_header_ranges(&mut self,parts:u64,base:u64,index_credit:u64) {
        if self.explicit_request_limit || parts<=base {return;}
        // A legal COM marker can place SOD outside the SOT probe. Reserve a
        // bounded marker-discovery allowance before selective eligibility is known.
        self.requests_limit=self.requests_limit.max(base.max(parts.saturating_mul(2).min(16_384)).saturating_add(index_credit));
    }
    fn allow_prefix_ranges(&mut self,parts:u64,base:u64,index_credit:u64) {
        if self.explicit_request_limit || parts<=base {return;}
        let prefix=base.max(parts.saturating_mul(3).min(16_384));
        self.requests_limit=self.requests_limit.max(prefix.saturating_add(index_credit));
    }
    fn admit_metadata(&mut self,bytes:usize)->bool {
        if self.metadata_bytes.saturating_add(bytes)>self.metadata_limit {return false;}
        self.metadata_bytes+=bytes;true
    }
    fn pin_small_object(&mut self, blocks: HashMap<u64, Arc<[u8]>>) {
        if blocks.values().map(|block| block.len()).sum::<usize>() <= 8 * 1024 * 1024 {
            self.pinned = blocks;
        }
    }
    // Only retain blocks already fetched by this request. Missing blocks
    // abort retention; this helper never performs network reads.
    fn pin_recent_range(&mut self,block_size:u64,range:Range<u64>)->bool {
        if !self.can_pin_ranges(block_size,&[(true,range.clone())]) {return false;}
        let mut blocks=HashMap::new();
        for block in range.start/block_size..range.end.div_ceil(block_size) {
            let offset=block*block_size;
            let Some(data)=self.pinned.get(&offset).cloned().or_else(||self.recent.get(block_size,offset)) else{return false;};
            blocks.insert(offset,data);
        }
        self.pin_packet_blocks(blocks);true
    }
    fn pin_packet_blocks(&mut self, blocks: HashMap<u64,Arc<[u8]>>) {
        let extra = blocks.iter().filter(|(offset,_)| !self.pinned.contains_key(offset)).map(|(_,data)| data.len()).sum::<usize>();
        if self.pinned.values().map(|data| data.len()).sum::<usize>()+extra <= 8*1024*1024 {
            self.pinned.extend(blocks);
        }
    }
    fn can_pin_ranges(&self, block_size:u64, spans:&[(bool,Range<u64>)])->bool {
        let mut bytes=self.pinned.values().map(|data|data.len() as u64).sum::<u64>();
        let mut missing=HashSet::new();
        for (_,span) in spans.iter().filter(|(keep,_)|*keep) {
            for block in span.start/block_size..span.end.div_ceil(block_size) {
                let offset=block*block_size;
                if !self.pinned.contains_key(&offset) && missing.insert(offset) {
                    bytes=bytes.saturating_add(block_size);
                    if bytes>8*1024*1024 {return false;}
                }
            }
        }
        true
    }
    #[cfg(all(feature="grok",unix))]
    pub(crate) fn finish_pipeline(&mut self)->Result<Option<(Arc<image::RgbaImage>,f64,(u32,u32))>>{self.pipeline.take().map(pipeline::Pipeline::finish).transpose().map(Option::flatten)}
    pub fn check(&self) -> Result<()> {
        ensure!(self.cancel.as_ref().is_none_or(|c| !*c.borrow()), "S3 request superseded");
        ensure!(Instant::now() < self.deadline, "S3 range-read deadline exceeded");
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub image_origin: (u32,u32),
    pub width: u32, pub height: u32, pub components: u8, pub bit_depth: u8,
    pub levels: u8, pub layers: u16, pub layout: NativeTileLayoutHint,
    pub progression: u8,
    pub is_ht: bool,
    cod: Vec<u8>,
    first_part: u64,
    codestream_end: u64,
    packet_compatible: bool,
    prefix_compatible: bool,
    payload_digest: [u8; 32],
}
impl Header {
    fn tile_bounds(&self, tile: u16) -> (u32,u32,u32,u32) {
        let tx = u32::from(tile) % self.layout.columns;
        let ty = u32::from(tile) / self.layout.columns;
        let x = self.layout.origin_x.saturating_add(tx.saturating_mul(self.layout.width));
        let y = self.layout.origin_y.saturating_add(ty.saturating_mul(self.layout.height));
        (x.max(self.image_origin.0), y.max(self.image_origin.1),
            x.saturating_add(self.layout.width).min(self.image_origin.0+self.width),
            y.saturating_add(self.layout.height).min(self.image_origin.1+self.height))
    }
}
#[derive(Debug,Clone)]
struct HeaderPayload { main: Vec<u8>, container: Vec<u8>, tlm: Vec<(Option<u16>, u32)>, jpip:Vec<jpip_index::Entry>, jpip_locator:Option<(jpip_index::Locator,u64)> }
impl HeaderPayload {
    fn digest(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        for bytes in [&self.main, &self.container] { hash.update((bytes.len() as u64).to_be_bytes()); hash.update(bytes); }
        for &(tile, length) in &self.tlm {
            hash.update(tile.map(u32::from).unwrap_or(u32::MAX).to_be_bytes()); hash.update(length.to_be_bytes());
        }
        hash.finalize().into()
    }
    fn bytes(&self) -> usize {
        self.main.capacity() + self.container.capacity() + self.tlm.capacity() * std::mem::size_of::<(Option<u16>, u32)>() + self.jpip.capacity()*std::mem::size_of::<jpip_index::Entry>()
    }
}
#[derive(Default)]
struct HeaderCacheState { entries: HashMap<String, (Arc<HeaderPayload>, usize, u64)>, bytes: usize, clock: u64 }
struct HeaderCache { limit: usize, state: Mutex<HeaderCacheState> }
impl HeaderCache {
    fn shared() -> &'static Self {
        static CACHE: std::sync::OnceLock<HeaderCache> = std::sync::OnceLock::new();
        CACHE.get_or_init(|| Self { limit: std::env::var("RIVER_S3_HEADER_CACHE_BYTES").ok().and_then(|value| value.parse().ok())
            .unwrap_or(32 * 1024 * 1024), state: Mutex::new(HeaderCacheState::default()) })
    }
    fn get(&self, identity: &str) -> Option<Arc<HeaderPayload>> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.clock += 1; let clock = state.clock;
        let entry = state.entries.get_mut(identity)?; entry.2 = clock;
        Some(entry.0.clone())
    }
    fn put(&self, identity: &str, payload: Arc<HeaderPayload>) {
        let bytes = payload.bytes();
        if self.limit == 0 || bytes > self.limit { return; }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(old) = state.entries.remove(identity) { state.bytes -= old.1; }
        while state.bytes + bytes > self.limit || state.entries.len() >= MAX_HEADER_ENTRIES {
            let Some(oldest) = state.entries.iter().min_by_key(|(_, entry)| entry.2).map(|(id, _)| id.clone()) else { break; };
            let old = state.entries.remove(&oldest).unwrap(); state.bytes -= old.1;
        }
        state.clock += 1; let clock = state.clock;
        state.entries.insert(identity.into(), (payload, bytes, clock)); state.bytes += bytes;
    }
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct TilePart { tile: u16, offset: u64, length: u64, ordinal: u8, total: u8 }
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct PartIndex { parts: Vec<TilePart>, cursor: u64, complete: bool }

fn ready_parts(index:&PartIndex,selected:&HashSet<u16>)->Option<Vec<TilePart>> {
    if !index.complete && index.parts.len()<selected.len() {return None;}
    let parts=index.parts.iter().filter(|part|selected.contains(&part.tile)).cloned().collect::<Vec<_>>();
    if !index.complete {
        // Validate each selected tile once. Keep the returned parts in their
        // original codestream order; only the temporary references are sorted.
        let mut ordered=parts.iter().collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|part|(part.tile,part.ordinal));
        let mut at=0;let mut tiles=0;
        while at<ordered.len() {
            let first=ordered[at];let mut end=at+1;
            while end<ordered.len() && ordered[end].tile==first.tile {end+=1;}
            let total=usize::from(first.total);
            if total==0 || end-at!=total || ordered[at..end].iter().enumerate()
                .any(|(ordinal,part)|part.total!=first.total || usize::from(part.ordinal)!=ordinal) {return None;}
            tiles+=1;at=end;
        }
        if tiles!=selected.len() {return None;}
    }
    Some(parts)
}

#[derive(Debug)]
pub struct RemoteSource {
    #[cfg(all(feature="grok",unix))] curl:Option<curl_transport::Source>,
    #[cfg(all(feature="grok",unix))] native_max_retries:usize,
    pub source_id: String,
    pub id: String,
    pub key: String,
    pub meta: ObjectMeta,
    pub header: Header,
    pub sequential_prefix: bool,
    /// SOT discovery uses the shared mutable index rather than explicit TLM entries.
    pub serial_index: bool,
    pub preparation_gate:Arc<tokio::sync::Mutex<()>>,
    header_payload: Mutex<std::sync::Weak<HeaderPayload>>,
    store: Arc<dyn ObjectStore>,
    location: Path,
    runtime: tokio::runtime::Handle,
    cache: Arc<RangeCache>,
    cache_key: Arc<str>,
    pub(crate) local_path:Option<std::path::PathBuf>,
    block_size: u64,
    network_min_us: Arc<AtomicU64>,
    read_profile: Arc<crate::read_policy::Profile>,
    fetch_lock: tokio::sync::Mutex<()>,
    compatibility_lock: Mutex<()>,
    parts: Mutex<PartIndex>,
    speculative_index:Mutex<index_shards::State>,
}

impl RemoteSource {
    fn fetch_admission<'a>(&'a self,context:&ReadContext)->Result<tokio::sync::MutexGuard<'a,()>> {
        context.check()?;
        // Tokio reserves the permit for queued waiters, so this fast path
        // cannot barge ahead of a payload. Avoid re-entering the runtime for
        // every uncontended SOT read.
        if let Ok(guard)=self.fetch_lock.try_lock() {return Ok(guard);}
        self.runtime.block_on(fetch_gate(&self.fetch_lock,context.deadline,context.cancel.clone()))
    }
    fn native_selection(&self,tiles:&[(u32,u32)],source_tile:u32)->Result<HashSet<u16>> {
        let h=&self.header;let mut selected=HashSet::new();
        for &(x,y) in tiles {
            let x0=x.saturating_mul(source_tile).saturating_add(h.image_origin.0);let y0=y.saturating_mul(source_tile).saturating_add(h.image_origin.1);
            let x1=x0.saturating_add(source_tile).min(h.image_origin.0+h.width);let y1=y0.saturating_add(source_tile).min(h.image_origin.1+h.height);
            for ty in (y0-h.layout.origin_y)/h.layout.height..(y1-h.layout.origin_y).div_ceil(h.layout.height) {
                for tx in (x0-h.layout.origin_x)/h.layout.width..(x1-h.layout.origin_x).div_ceil(h.layout.width) {
                    selected.insert(u16::try_from(ty*h.layout.columns+tx).context("remote tile index exceeds JPEG 2000 limit")?);
                }
            }
        }
        ensure!(!selected.is_empty(),"empty remote tile request");Ok(selected)
    }

    pub async fn preparation_admission(&self,tiles:&[(u32,u32)],source_tile:u32)->Option<tokio::sync::OwnedMutexGuard<()>> {
        if !self.serial_index || self.meta.size<64*1024*1024 {return None;}
        let selected=self.native_selection(tiles,source_tile).ok();
        loop {
            if selected.as_ref().is_some_and(|selected|index_cache::ready(&self.id,&self.header,self.meta.size,selected).is_some()) {return None;}
            tokio::select! {
                guard=self.preparation_gate.clone().lock_owned()=>return Some(guard),
                _=tokio::time::sleep(Duration::from_millis(20))=>{}
            }
        }
    }
    /// Compatibility route for valid layouts which cannot be represented by
    /// our packet subset reader. Stream into disk, never a source-sized Vec.
    pub fn cached_compatibility_file(&self, context: &mut ReadContext) -> Option<Arc<tempfile::NamedTempFile>> {
        let file = CompatibilityCache::shared().lock().unwrap_or_else(|error| error.into_inner()).get(&self.id)?;
        context.stats.cache_bytes += self.meta.size;
        Some(file)
    }

    pub fn compatibility_file(&self, context: &mut ReadContext) -> Result<Arc<tempfile::NamedTempFile>> {
        // Serialize misses only for this source; unrelated objects transfer concurrently.
        let _flight = lock_cancellable(&self.compatibility_lock, context)?;
        context.check()?;
        if let Some(file) = self.cached_compatibility_file(context) { return Ok(file); }
        let limit = env_limit("RIVER_S3_COMPATIBILITY_BYTES", 2 * 1024 * 1024 * 1024) as u64;
        ensure!(self.meta.size <= limit, "source exceeds the disk-backed compatibility limit ({limit} bytes); configure RIVER_S3_COMPATIBILITY_BYTES for this source");
        context.check()?;
        let root = std::env::var_os("RIVER_S3_CACHE_ROOT").map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("river-s3-cache")).join("range-views");
        std::fs::create_dir_all(&root)?;
        let payload = self.acquire_header(context)?;
        let suffix = if payload.container.is_empty() { ".j2k" } else { ".jp2" };
        let mut file = tempfile::Builder::new().prefix("river-compatible-").suffix(suffix).tempfile_in(root)?;
        let mut cancel = context.cancel.clone();
        // Compatibility transfer has a separate byte/deadline policy. Preserve
        // its allowed object sizes while explicit range caps remain absolute.
        if !context.explicit_request_limit {
            context.requests_limit=context.requests_limit.max(context.wire_budget.used().saturating_add(self.meta.size.div_ceil(1024*1024)));
        }
        let range_client=self.range_client(context);
        let mut requests = 0;
        let bytes = self.runtime.block_on(async {
            let deadline=Instant::now()+Duration::from_secs(300);
            let read_cancel=cancel.clone();
            let concurrency=env_limit("RIVER_S3_COMPATIBILITY_CONCURRENCY",4).clamp(1,8);
            let transfer = async {
                let mut bytes = 0u64;
                // Preserve file order with bounded 1 MiB chunks per lane.
                // Use the same pinned, bounded and cancellable range reader as
                // selective preparation, including its individual timeout.
                let mut chunks=futures_util::stream::iter((0..self.meta.size).step_by(1024*1024).map(|start| {
                    let cancel = read_cancel.clone();
                    let range_client=range_client.clone();
                    async move {
                        let (mut prefix, missing, suffix) = self.cache.compatibility_edges(
                            &self.cache_key, self.block_size, start..(start+1024*1024).min(self.meta.size));
                        let cached = prefix.len()+suffix.len();
                        let mut fetched = 0;
                        if !missing.is_empty() {
                            let body = range_client.fetch(missing,deadline,cancel).await?;
                            fetched = body.len();
                            prefix.extend_from_slice(&body);
                        }
                        prefix.extend_from_slice(&suffix);
                        Ok::<_,anyhow::Error>((prefix, fetched, cached))
                    }
                })).buffered(concurrency);
                while let Some((chunk, fetched, cached)) = chunks.try_next().await? {
                    context.stats.cache_bytes += cached as u64;
                    context.stats.bytes += fetched as u64;
                    bytes = bytes.checked_add(chunk.len() as u64).context("source size overflow")?;
                    ensure!(bytes <= self.meta.size, "S3 compatibility response exceeds source extent");
                    file.write_all(&chunk)?;
                    requests += u64::from(fetched != 0);
                }
                ensure!(bytes == self.meta.size, "truncated S3 compatibility response");
                Ok::<_, anyhow::Error>(bytes)
            };
            let cancelled = async {
                if let Some(receiver) = &mut cancel {
                    if *receiver.borrow() { return; }
                    let _ = receiver.changed().await;
                } else { std::future::pending::<()>().await; }
            };
            tokio::select! {
                result = tokio::time::timeout(Duration::from_secs(300), transfer) => result.context("S3 compatibility transfer timed out")?,
                _ = cancelled => bail!("S3 request superseded"),
            }
        })?;
        file.flush()?;
        context.stats.requests += requests;
        // The compatibility route has its own bounded transfer deadline.
        context.deadline = Instant::now() + Duration::from_secs(30);
        context.check()?;
        let file = Arc::new(file);
        let cache_limit = std::env::var("RIVER_S3_COMPATIBILITY_CACHE_BYTES").ok().and_then(|value| value.parse().ok())
            .unwrap_or(2 * 1024 * 1024 * 1024);
        CompatibilityCache::shared().lock().unwrap_or_else(|error| error.into_inner())
            .put(self.id.clone(), file.clone(), bytes, cache_limit);
        Ok(file)
    }

    pub fn open_file(path: &std::path::Path, runtime: tokio::runtime::Handle, cache: Arc<RangeCache>) -> Result<(Arc<Self>, ReadStats)> {
        use sha2::{Digest, Sha256};
        let absolute = std::fs::canonicalize(path).context("local image is unavailable")?;
        let store: Arc<dyn ObjectStore> = Arc::new(object_store::local::LocalFileSystem::new_with_prefix(
            absolute.parent().context("local image has no parent directory")?)?);
        let location = Path::from(absolute.file_name().and_then(|name| name.to_str()).context("local image name is not Unicode")?);
        let meta = runtime.block_on(store.head(&location))?;
        let identity = format!("local-header-{}",hex::encode(Sha256::digest(serde_json::to_vec(
            &(&absolute,&meta.e_tag,meta.size,meta.last_modified))?)));
        let (source, stats) = Self::open_reader(format!("file:{}",hex::encode(Sha256::digest(absolute.parent().unwrap().to_string_lossy().as_bytes()))),identity,store,location,meta,runtime,cache,None,Some(absolute))?;
        Ok((source,stats))
    }

    pub fn probe_file(path:&std::path::Path,runtime:tokio::runtime::Handle,cache:Arc<RangeCache>)->Result<(Header,ReadStats)> {
        Self::open_file(path,runtime,cache).map(|(reader,stats)|(reader.header.clone(),stats))
    }
    pub fn local_ranges_supported(&self)->bool {
        self.local_path.is_some() && self.meta.size>=32*1024*1024 && !self.header.is_ht
            && (self.header.packet_compatible || self.header.prefix_compatible)
    }
    pub fn open(source_id: String, id: String, store: Arc<dyn ObjectStore>, location: Path,
        meta: ObjectMeta, runtime: tokio::runtime::Handle, cache: Arc<RangeCache>) -> Result<(Arc<Self>, ReadStats)> {
        Self::open_reader(source_id,id,store,location,meta,runtime,cache,None,None)
    }
    pub fn open_from_probe(source_id:String,id:String,store:Arc<dyn ObjectStore>,location:Path,
        probe:InitialRead,runtime:tokio::runtime::Handle,cache:Arc<RangeCache>)->Result<(Arc<Self>,ReadStats)> {
        Self::open_reader(source_id,id,store,location,probe.meta,runtime,cache,Some(probe.seed),None)
    }
    #[cfg(all(feature="grok",unix))]
    pub(crate) fn open_s3_from_probe(source_id:String,id:String,store:Arc<object_store::aws::AmazonS3>,location:Path,
        probe:InitialRead,runtime:tokio::runtime::Handle,cache:Arc<RangeCache>,region:String,allow_http:bool,max_retries:usize)->Result<(Arc<Self>,ReadStats)> {
        let (mut source,stats)=Self::open_from_probe(source_id,id,store.clone(),location.clone(),probe,runtime.clone(),cache)?;
        Arc::get_mut(&mut source).context("S3 reader was shared before transport initialization")?.curl=Some(curl_transport::Source::new(store,region,allow_http));
        Arc::get_mut(&mut source).unwrap().native_max_retries=max_retries;
        Ok((source,stats))
    }
    fn open_reader(source_id:String,id:String,store:Arc<dyn ObjectStore>,location:Path,
        meta:ObjectMeta,runtime:tokio::runtime::Handle,cache:Arc<RangeCache>,seed:Option<HeaderSeed>,local_path:Option<std::path::PathBuf>)->Result<(Arc<Self>,ReadStats)> {
        ensure!(meta.e_tag.is_some() || meta.version.is_some(), "S3 source must expose an ETag or version for consistent range reads");
        // Header parsing uses the same reader and shared cache as later passes.
        let cache_key = Arc::from(id.as_str());
        let read_profile=crate::read_policy::profile(&source_id);
        let mut source = Self {
            #[cfg(all(feature="grok",unix))] curl:None,
            #[cfg(all(feature="grok",unix))] native_max_retries:2,
            local_path, read_profile, source_id, id, key: location.as_ref().to_owned(), meta, store, location, runtime, cache, cache_key, block_size: BLOCK,
            sequential_prefix: false, serial_index: false, preparation_gate:Arc::new(tokio::sync::Mutex::new(())), network_min_us: Arc::new(AtomicU64::new(u64::MAX)),
            header: Header { image_origin: (0,0), width: 0, height: 0, components: 0, bit_depth: 0, levels: 0, layers: 0,
                layout: NativeTileLayoutHint { origin_x: 0, origin_y: 0, width: 0, height: 0, columns: 0, rows: 0 },
                progression: 0, is_ht: false, cod: vec![], first_part: 0, codestream_end: 0,
                packet_compatible: false, prefix_compatible: false, payload_digest: [0; 32] },
            header_payload: Mutex::new(std::sync::Weak::new()),
            fetch_lock: tokio::sync::Mutex::new(()), compatibility_lock: Mutex::new(()), speculative_index:Mutex::new(index_shards::State::default()), parts: Mutex::new(PartIndex::default()) };
        let mut context = ReadContext::new(None);
        context.limit = MAX_HEADER as u64 + 2 * BLOCK;
        if let Some(seed)=seed {
            context.limit+=seed.stats.bytes;
            context.stats=seed.stats;context.wire_budget=seed.budget;
            source.network_min_us.store(seed.elapsed_us,Ordering::Relaxed);
            let elapsed=Duration::from_micros(seed.elapsed_us);source.read_profile.record(context.stats.bytes,elapsed,elapsed);
            if let Some(bytes)=seed.prefix {
                let blocks=source.cache.put(&source.cache_key,source.block_size,0,&bytes);
                if source.cache.limit<8*1024*1024 {context.recent.remember(source.block_size,&blocks);}
            }
        }
        let (header, payload) = source.read_header(&mut context)?;
        // Keep an already materialized, version-keyed index when recreating
        // a reader for the same immutable header.
        let payload=HeaderCache::shared().get(&source.id).filter(|cached|cached.jpip_locator.is_none() && cached.digest()==header.payload_digest).unwrap_or(payload);
        source.header = header;
        source.sequential_prefix=payload.tlm.is_empty() && source.header.prefix_compatible && source.header.layers==1;
        source.serial_index=payload.tlm.is_empty() || payload.tlm.iter().any(|(tile,_)|tile.is_none());
        *source.header_payload.get_mut().unwrap() = Arc::downgrade(&payload);
        HeaderCache::shared().put(&source.id, payload);
        // Spatially interleaved PCRL/CPRL packets benefit from finer reads.
        // Other layouts retain larger blocks to avoid unnecessary cache nodes.
        if source.header.packet_compatible && source.header.progression >= 3 { source.block_size = 256; }
        source.parts.get_mut().unwrap().cursor = source.header.first_part;
        Ok((Arc::new(source), context.stats))
    }

    pub fn minimum_range_latency_us(&self)->u64 {self.network_min_us.load(Ordering::Relaxed)}
    fn planning_latency_us(&self)->u64 {
        let historical=self.network_min_us.load(Ordering::Relaxed);
        // A historical minimum must not classify a now-distant/loaded source as nearby.
        let recent=self.read_profile.snapshot().latency_ms.map_or(0,|ms|(ms*1000.0).ceil() as u64);
        historical.max(recent)
    }

    #[track_caller]
    fn read(&self, range: Range<u64>, context: &mut ReadContext) -> Result<Vec<u8>> {
        self.read_blocks(range,context,false)
    }
    #[track_caller]
    fn read_packet_probe(&self,range:Range<u64>,context:&mut ReadContext)->Result<Vec<u8>> {
        self.read_blocks(range,context,true)
    }
    #[track_caller]
    fn read_blocks(&self, range: Range<u64>, context: &mut ReadContext, retain:bool) -> Result<Vec<u8>> {
        context.check()?;
        let retain=retain && self.cache.limit<8*1024*1024;
        ensure!(range.start <= range.end && range.end <= self.meta.size, "JPEG 2000 read exceeds object size");
        let length = usize::try_from(range.end - range.start)?;
        ensure!(length <= context.limit as usize, "S3 compressed-read budget exceeded; source needs a packet index or smaller viewport");
        let mut output = Vec::with_capacity(length);
        let mut cursor = range.start;
        while cursor < range.end {
            context.check()?;
            let block_start = cursor / self.block_size * self.block_size;
            if let Some(block) = context.pinned.get(&block_start).cloned().or_else(||context.recent.get(self.block_size,block_start))
                .or_else(|| self.cache.get(&self.cache_key, self.block_size, block_start)) {
                if retain && !context.pinned.contains_key(&block_start) {
                    context.pin_packet_blocks(HashMap::from([(block_start,block.clone())]));
                }
                let within = (cursor - block_start) as usize;
                let count = (range.end - cursor).min(block.len() as u64 - within as u64) as usize;
                ensure!(count > 0, "invalid cached S3 block");
                output.extend_from_slice(&block[within..within + count]);
                cursor += count as u64;
                context.stats.cache_bytes += count as u64;
                continue;
            }
            #[cfg(all(feature="grok",unix))] if context.cache_only {return Err(pipeline::PrefixNotCached.into());}
            // Coalesce concurrent misses for this object, not unrelated objects.
            let _fetch = self.fetch_admission(context)?;
            if self.cache.get(&self.cache_key, self.block_size, block_start).is_some() { continue; }
            context.check()?;
            let mut end = range.end.div_ceil(self.block_size).saturating_mul(self.block_size)
                .min(self.meta.size).min(block_start + 1024 * 1024);
            let mut probe = block_start + self.block_size;
            while probe < end {
                if self.cache.get(&self.cache_key, self.block_size, probe).is_some() { end = probe; break; }
                probe += self.block_size;
            }
            ensure!(context.stats.bytes + end - block_start <= context.limit,
                "S3 compressed-read budget exceeded; source needs a packet index or smaller viewport (usedBytes={}, nextRangeBytes={}, limitBytes={}, phase={})",context.stats.bytes,end-block_start,context.limit,if context.trace_index.is_some() {"sotDiscovery"} else {"headerOrPacket"});
            ensure!(context.stats.requests < context.requests_limit, "S3 range request limit exceeded; source needs a tile-part index (used={}, limit={}, phase={})",context.stats.requests,context.requests_limit,if context.trace_index.is_some() {"sotDiscovery"} else {"headerOrPacket"});
            let opts = GetOptions { range: Some(GetRange::Bounded(block_start..end)),
                if_match: self.meta.e_tag.clone(), version: self.meta.version.clone(), ..Default::default() };
            let store = self.store.clone();
            let location = self.location.clone();
            let mut cancel = context.cancel.clone();
            let remaining = context.deadline.saturating_duration_since(Instant::now());
            let mut range_trace=read_trace::Read::begin(context.trace_id,&self.id,std::panic::Location::caller(),
                if context.main_header {"mainHeader"} else if context.trace_index.is_some() {"sotDiscovery"} else if retain {"packetHeader"} else {"headerOrPacket"},
                block_start..end,self.block_size,None,context.trace_index,"serial");
            let admission=self.read_profile.acquire();
            let _source_permit=self.runtime.block_on(async {let cancelled=async {if let Some(r)=&mut cancel {if !*r.borrow(){let _=r.changed().await;}}else{std::future::pending::<()>().await;}};
                tokio::select! {r=tokio::time::timeout(remaining,admission)=>r.context("S3 source admission timed out"),_=cancelled=>Err(anyhow::anyhow!("S3 request superseded"))}
            })?;
            range_trace.admitted();
            // Queueing consumes the same request deadline as network transfer.
            context.check()?;
            let remaining=context.deadline.saturating_duration_since(Instant::now());
            context.wire_budget.set_limit(context.requests_limit);
            let before_curl=context.curl_requests;
            let network_started = Instant::now();
            #[cfg(all(feature="grok",unix))]
            let native = if let Some(builder)=self.curl.as_ref().filter(|_| {
                match context.serial_transport {
                    crate::read_policy::SerialTransport::Curl=>true,
                    crate::read_policy::SerialTransport::Auto=>context.auto_serial.allow(self.serial_index,u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows),self.planning_latency_us(),crate::source_admission::active_preparations()),
                    crate::read_policy::SerialTransport::Sdk=>false,
                }
            }) {
                let config=if context.serial_transport==crate::read_policy::SerialTransport::Auto {
                    let available=if context.auto_serial.ready {Ok(())}else{curl_transport::initialize()};
                    match available.and_then(|_|builder.config(&self.location,&self.meta,&self.runtime)) {
                        Ok(c)=>{context.auto_serial.ready=true;Some(c)},Err(_)=>{context.auto_serial.disabled=true;None}
                    }
                }else{Some(builder.config(&self.location,&self.meta,&self.runtime)?)};
                if config.is_none(){None}else{
                let config=config.unwrap();
                if context.curl_lease.is_none(){context.curl_lease=curl_transport::lease()?;}
                let deadline=context.deadline.min(Instant::now()+Duration::from_secs(10));let cancel=context.cancel.clone();
                if context.curl_lease.is_some() {
                    range_trace.native_parent();
                    let bytes=(||->Result<Vec<u8>> {
                        for attempt in 0..=self.native_max_retries {
                            context.check()?;
                            ensure!(context.stats.requests<context.requests_limit,"native S3 retry would exceed the request range budget");
                            ensure!(context.stats.bytes+end-block_start<=context.limit,"native S3 retry would exceed the request byte budget");
                            context.wire_budget.claim()?;
                            let mut attempt_trace=read_trace::Read::begin(context.trace_id,&self.id,std::panic::Location::caller(),"nativeAttempt",block_start..end,self.block_size,None,context.trace_index,"curl");
                            let result=context.curl_lease.as_mut().unwrap().fetch(&config,&self.runtime,block_start..end,deadline,cancel.clone());
                            match result {
                                Ok(fetched)=>{
                                    attempt_trace.finish(fetched.bytes.len() as u64,true,Some(fetched.requests));
                                    context.stats.requests+=fetched.requests;context.curl_requests+=fetched.requests;context.stats.bytes+=fetched.bytes.len() as u64;
                                    self.read_profile.serial_counts(0,fetched.requests);
                                    ensure!(context.stats.requests<=context.requests_limit,"native S3 request exceeded its range budget");
                                    return Ok(fetched.bytes);
                                }
                                Err(error)=>{
                                    let failure=error.downcast_ref::<curl_transport::Failure>();
                                    attempt_trace.finish(failure.map_or(0,|f|f.bytes),false,failure.map(|f|f.requests));
                                    if let Some(f)=failure {context.stats.requests+=f.requests;context.curl_requests+=f.requests;context.stats.bytes+=f.bytes;self.read_profile.serial_counts(0,f.requests);}
                                    let Some(f)=failure.filter(|f|f.retryable && attempt<self.native_max_retries) else{return Err(error);};
                                    let delay=Duration::from_millis(f.retry_after_ms.max(100u64<<attempt));
                                    if Instant::now().checked_add(delay).is_none_or(|at|at>=deadline){return Err(error);}
                                    self.runtime.block_on(retry_delay(delay,cancel.clone()))?;
                                }
                            }
                        }
                        unreachable!("native retry loop always completes within configured attempts")
                    })()?;Some(bytes)
                }else{None}}
            }else{None};
            #[cfg(not(all(feature="grok",unix)))] let native:Option<Vec<u8>>=None;
            let used_native=native.is_some();
            let (data,header_time) = if let Some(bytes)=native {(bytes,None)}else{self.runtime.block_on(async {
                let get = async {
                    let result = context.wire_budget.scope(range_trace.scope(store.get_opts(&location, opts))).await?;
                    ensure!(result.range == (block_start..end), "S3 endpoint ignored or changed the requested byte range");
                    let header_time=network_started.elapsed();
                    let bytes = bounded_body(result, end - block_start).await?;
                    Ok::<_, anyhow::Error>((bytes,Some(header_time)))
                };
                let cancelled = async {
                    if let Some(receiver) = &mut cancel {
                        if *receiver.borrow() { return; }
                        let _ = receiver.changed().await;
                    } else { std::future::pending::<()>().await; }
                };
                tokio::select! {
                    result = tokio::time::timeout(remaining.min(Duration::from_secs(10)), get) => result.context("S3 range timed out")?,
                    _ = cancelled => bail!("S3 request superseded"),
                }
            })?};
            range_trace.finish(data.len() as u64,true,if used_native {Some(context.curl_requests-before_curl)}else{None});
            self.network_min_us.fetch_min(network_started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64, Ordering::Relaxed);
            let elapsed=network_started.elapsed();
            if let Some(headers)=header_time {range_trace.transfer_timing(headers,elapsed);}
            self.read_profile.record_transfer(data.len() as u64,header_time.or_else(||(data.len()<=4096).then_some(elapsed)),elapsed);
            if !used_native {
                context.stats.bytes += data.len() as u64;context.stats.requests += 1;
                if self.local_path.is_none(){self.read_profile.serial_counts(1,0);}
            }
            let blocks=self.cache.put(&self.cache_key, self.block_size, block_start, &data);
            if self.cache.limit<8*1024*1024 {context.recent.remember(self.block_size,&blocks);}
            if retain {context.pin_packet_blocks(blocks.into_iter().collect());}
            // Copy directly as a tiny configured cache may evict this block.
            let within = (cursor - block_start) as usize;
            let count = (range.end - cursor).min(data.len() as u64 - within as u64) as usize;
            output.extend_from_slice(&data[within..within + count]);
            cursor += count as u64;
        }
        Ok(output)
    }

    fn read_header(&self, context: &mut ReadContext) -> Result<(Header, Arc<HeaderPayload>)> {
        let previous=context.main_header;context.main_header=true;
        let result=self.read_header_inner(context);
        context.main_header=previous;result
    }
    fn read_header_inner(&self, context: &mut ReadContext) -> Result<(Header, Arc<HeaderPayload>)> {
        let first = self.read(0..self.meta.size.min(16), context)?;
        let mut start = 0;
        let mut codestream_end = self.meta.size;
        let mut container = Vec::new();
        let mut jpip_locator=None;
        if first.get(..2) != Some(&[0xff, 0x4f]) {
            let mut boxes = 0;
            loop {
                boxes += 1;
                ensure!(boxes < 4096, "too many JP2 boxes");
                let bytes = self.read(start..start + 16, context)?;
                let mut size = be32(&bytes, 0)? as u64;
                let header = if size == 1 { size = be64(&bytes, 8)?; 16 } else { 8 };
                if size == 0 { size = self.meta.size - start; }
                ensure!(size >= header && start.checked_add(size).is_some_and(|end| end <= self.meta.size), "invalid JP2 box length");
                if &bytes[4..8] == b"jp2c" { codestream_end = start + size; start += header; break; }
                if &bytes[4..8]==b"iptr" && size-header==16 {
                    let pointer=self.read(start+header..start+size,context)?;
                    jpip_locator=Some(jpip_index::Locator::Root {offset:be64(&pointer,0)?,length:be64(&pointer,8)?});
                } else if &bytes[4..8]==b"cidx" {
                    jpip_locator=Some(jpip_index::Locator::Codestream {offset:start,length:size});
                }
                if [b"jP  ", b"ftyp", b"jp2h"].iter().any(|kind| bytes[4..8] == kind[..]) {
                    ensure!(container.len() as u64 + size <= MAX_HEADER as u64, "JP2 essential metadata is too large");
                    container.extend(self.read(start..start + size, context)?);
                }
                start += size;
            }
        }
        ensure!(self.read(start..start + 2, context)? == [0xff, 0x4f], "missing JPEG 2000 SOC marker");
        let mut main = vec![0xff, 0x4f];
        let mut cursor = start + 2;
        let mut siz = None;
        let mut cod = None;
        let mut qcd = None;
        let mut compatible = true;
        let mut component_coding = Vec::new();
        let mut tlm_segments = BTreeMap::new();
        let mut markers = 0;
        loop {
            markers += 1;
            ensure!(markers <= 4096, "too many JPEG 2000 main-header markers");
            let marker = self.read(cursor..cursor + 4, context)?;
            let code = be16(&marker, 0)?;
            if code == 0xff90 { break; }
            ensure!(code & 0xff00 == 0xff00 && code != 0xffd9, "invalid JPEG 2000 main header");
            let length = be16(&marker, 2)? as u64;
            ensure!(length >= 2 && cursor.checked_add(2 + length).is_some_and(|end| end <= self.meta.size), "invalid main-header marker extent");
            // COM is descriptive text/binary data, not coding parameters.
            // Its extent is known without fetching/retaining the payload.
            if code == 0xff64 {
                ensure!(length >= 4, "truncated JPEG 2000 comment marker");
                cursor += 2 + length;
                continue;
            }
            ensure!(length >= 2 && main.len() as u64 + length + 2 <= MAX_HEADER as u64, "JPEG 2000 main header exceeds limit");
            let required_end=cursor+2+length;
            let policy=crate::read_policy::settings();
            let read_end=tlm_header_read_end(code,cursor,required_end,codestream_end,
                policy.mode==crate::read_policy::Mode::Adaptive && u64::from(policy.max_overfetch_ratio).min(crate::read_policy::explicit_overfetch_cap().unwrap_or(16))>=2,
                context.limit.saturating_sub(context.stats.bytes),self.block_size);
            let mut segment = self.read(cursor..read_end, context)?;
            // Read-ahead is cache warming only. It never supplies guessed
            // marker boundaries or changes a TLM segment's exact contents.
            segment.truncate((required_end-cursor) as usize);
            match code {
                0xff51 => { ensure!(siz.is_none(), "duplicate SIZ marker"); siz = Some(segment[4..].to_vec()); }
                0xff52 => { ensure!(cod.is_none(), "duplicate COD marker"); cod = Some(segment[4..].to_vec()); }
                0xff5c => qcd = Some(segment[4..].to_vec()),
                0xff55 => { ensure!(segment.len() >= 6, "truncated TLM marker"); tlm_segments.insert(segment[4], segment[5..].to_vec()); }
                // An identical component override leaves packet geometry
                // unchanged. Decide after COD/SIZ are available, independent
                // of marker order; retain every original marker in `main`.
                0xff53 => component_coding.push(segment[4..].to_vec()),
                // POC and packed packet headers need their own index planner.
                0xff5f | 0xff60 | 0xff57 => compatible = false,
                _ => {}
            }
            // TLM/PLM offsets refer to the original payload, not the subset.
            if code != 0xff55 && code != 0xff57 { main.extend(segment); }
            cursor += 2 + length;
        }
        let siz = siz.context("JPEG 2000 SIZ marker missing")?;
        let cod = cod.context("JPEG 2000 COD marker missing")?;
        ensure!(siz.len() >= 36 && cod.len() >= 10, "truncated SIZ/COD marker");
        let width = be32(&siz, 2)?.checked_sub(be32(&siz, 10)?).context("invalid image width")?;
        let height = be32(&siz, 6)?.checked_sub(be32(&siz, 14)?).context("invalid image height")?;
        let components = be16(&siz, 34)? as usize;
        ensure!(width > 0 && height > 0 && components > 0 && components <= 4 && siz.len() == 36 + 3 * components, "unsupported JPEG 2000 dimensions/components");
        let bit_depth = (0..components).map(|i| (siz[36 + i * 3] & 0x7f) + 1).max().unwrap();
        ensure!(bit_depth <= 16 && cod[5] < 16 && cod[6] <= 8 && cod[7] <= 8, "unsupported precision, resolution count or codeblock size");
        let tw = be32(&siz, 18)?; let th = be32(&siz, 22)?;
        let tx = be32(&siz, 26)?; let ty = be32(&siz, 30)?;
        ensure!(tw > 0 && th > 0 && tx <= be32(&siz, 10)? && ty <= be32(&siz, 14)?, "invalid tile grid");
        let layout = NativeTileLayoutHint { origin_x: tx, origin_y: ty, width: tw, height: th,
            columns: (be32(&siz, 2)? - tx).div_ceil(tw), rows: (be32(&siz, 6)? - ty).div_ceil(th) };
        compatible &= (0..components).all(|i| siz[37 + i * 3] == 1 && siz[38 + i * 3] == 1)
            && cod[0] & !7 == 0 && cod[1] <= 4;
        compatible &= component_coding.iter().all(|coc|identical_coc(coc,components as u8,&cod));
        ensure!(cod[0] & 1 == 0 || cod.len() >= 11 + cod[5] as usize, "truncated precinct dimensions");
        let layers = be16(&cod, 2)?;
        ensure!(layers > 0, "empty JPEG 2000 quality layers");
        ensure!(qcd.as_ref().is_some_and(|q| !q.is_empty()), "JPEG 2000 QCD marker missing");
        let is_ht = be16(&siz, 0)? & 0x4000 != 0 || cod[8] & 0x40 != 0;
        let prefix_compatible = compatible && matches!(cod[1], 0..=2) && (layers == 1 || cod[1] == 0)
            && cod[4] <= 1 && (if is_ht { cod[8] == 0x40 } else { cod[8] & 0xc0 == 0 }) && cod[9] <= 1;
        let mut tlm = vec![];
        for payload in tlm_segments.values() {
            if let Ok(entries) = parse_tlm(payload) { tlm.extend(entries); } else { tlm.clear(); break; }
        }
        let progression = cod[1];
        let levels = cod[5] + 1;
        let mut descriptor_cod = cod[..if cod[0] & 1 != 0 { 10 + usize::from(levels) } else { 10 }].to_vec();
        descriptor_cod.shrink_to_fit();
        // Metadata-only callers need dimensions, not an optional tile index.
        // Save the advertised locator and load it only when rendering needs it.
        let jpip_locator=if tlm.is_empty() && self.meta.size>env_limit("RIVER_S3_SMALL_OBJECT_BYTES",8*1024*1024) as u64 {jpip_locator.map(|locator|(locator,start))}else{None};
        let payload = Arc::new(HeaderPayload { main, container, tlm, jpip:vec![],jpip_locator });
        Ok((Header { image_origin: (be32(&siz,10)?,be32(&siz,14)?), width, height, components: components as u8, bit_depth, levels,
            layers, layout, progression, is_ht,
            cod: descriptor_cod, first_part: cursor, codestream_end,
            packet_compatible: compatible, prefix_compatible, payload_digest: payload.digest() }, payload))
    }

    fn acquire_header(&self, context: &mut ReadContext) -> Result<Arc<HeaderPayload>> {
        if let Some(payload) = HeaderCache::shared().get(&self.id) { return Ok(payload); }
        let mut weak = lock_cancellable(&self.header_payload, context)?;
        if let Some(payload) = weak.upgrade() { return Ok(payload); }
        let (header, payload) = self.read_header(context)?;
        ensure!(header == self.header,
            "reloaded header differs from immutable snapshot");
        *weak = Arc::downgrade(&payload);
        HeaderCache::shared().put(&self.id, payload.clone());
        Ok(payload)
    }
    fn acquire_render_header(&self,context:&mut ReadContext)->Result<Arc<HeaderPayload>> {
        let payload=self.acquire_header(context)?;
        if payload.jpip_locator.is_none() {return Ok(payload);}
        let mut weak=lock_cancellable(&self.header_payload,context)?;
        let current=HeaderCache::shared().get(&self.id).or_else(||weak.upgrade()).unwrap_or(payload);
        let Some((locator,start))=current.jpip_locator else{return Ok(current);};
        let mut loaded=(*current).clone();loaded.jpip_locator=None;
        let previous=context.main_header;context.main_header=true;
        let result=self.read_jpip_index(locator,start,self.header.codestream_end,self.header.first_part,u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows),context);
        context.main_header=previous;
        match result {
            Ok(entries)=>{tracing::info!(context=context.trace_id,optimization="embedded_tile_part_lookup",index_source="jpip_tpix",parts=entries.len(),action="validate_selected_sot","Embedded tile-part index available");loaded.jpip=entries;}
            Err(error)=>{context.check()?;tracing::info!(context=context.trace_id,optimization="embedded_tile_part_lookup",reason=%error,action="bounded_discovery","Optional JPIP index unavailable");}
        }
        let loaded=Arc::new(loaded);*weak=Arc::downgrade(&loaded);HeaderCache::shared().put(&self.id,loaded.clone());Ok(loaded)
    }

    #[tracing::instrument(name = "river.source.s3.tile_index", skip_all, fields(
        selected_tiles = selected.len(), index_source = tracing::field::Empty,
        discovered_parts = tracing::field::Empty, range_requests = tracing::field::Empty,
        range_bytes = tracing::field::Empty))]
    fn selected_parts(&self, selected: &HashSet<u16>, payload: &HeaderPayload, scan_ahead: u64, selected_prefix: u64, complete_parts:bool, whole_view:bool, context: &mut ReadContext) -> Result<Vec<TilePart>> {
        let before = context.stats.clone();
        let span = tracing::Span::current();
        let header = &self.header;
        // Environment policy is stable for a request; avoid checking it for
        // every header, and keep ordinary native grids on their existing path.
        let primary_chunks=u64::from(header.layout.columns)*u64::from(header.layout.rows)>1024
            && payload.tlm.is_empty()
            && !std::env::var("RIVER_S3_INDEX_PRIMARY_CHUNKS").is_ok_and(|v|v=="false" || v=="0");
        let tlm_offsets = tlm_offsets(header.first_part,header.codestream_end,&payload.tlm)?;
        if !payload.tlm.is_empty() {
            tracing::info!(context=context.trace_id,optimization="direct_tile_part_lookup",index_source="tlm",parts=payload.tlm.len(),action="validate_selected_sot","Using file tile-part index");
        }
        if !payload.tlm.is_empty() && payload.tlm.iter().all(|(tile, _)| tile.is_some()) {
            span.record("index_source", "tlm");
            let mut offset = header.first_part;
            let mut parts = vec![];
            let candidates = payload.tlm.iter().zip(&tlm_offsets).filter(|((tile,_),_)| selected.contains(&tile.unwrap())).collect::<Vec<_>>();
            for page in candidates.chunks(256) {
                let whole=page.iter().map(|(entry,offset)|(true,**offset..**offset+u64::from(entry.1))).collect::<Vec<_>>();
                let fuse=complete_parts && context.can_pin_ranges(self.block_size,&whole);
                let spans = if fuse {whole} else {page.iter().filter(|(entry,offset)| {
                    // Reuse a previously validated SOT only while this probe
                    // lies entirely inside its parsed header. Payload lookahead
                    // and complete-part requests must still fetch their bytes.
                    !tile_header_cache::Cache::shared().indexed(&self.id,**offset).is_some_and(|(part,payload_start)|
                        Some(part.tile)==entry.0 && part.length==u64::from(entry.1)
                        && offset.saturating_add(selected_prefix.max(12)).min(offset.saturating_add(u64::from(entry.1)))<=payload_start)
                }).map(|(entry,offset)| (true,**offset..(**offset+selected_prefix.max(12)).min(**offset+u64::from(entry.1)))).collect::<Vec<_>>()};
                let leases = self.prefetch_ranges(&spans,context,usize::MAX)?;
                context.pin_packet_blocks(leases);
            }
            for (position,&(tile, length)) in payload.tlm.iter().enumerate() {
                ensure!(length >= 14 && offset + length as u64 <= self.meta.size, "invalid TLM tile-part extent");
                if selected.contains(&tile.unwrap()) {
                    let part = self.part_at(offset, context)?;
                    ensure!(part.tile == tile.unwrap() && part.length == u64::from(length), "TLM does not match SOT");
                    read_trace::index_entry_from(context.trace_id,&self.id,position,part.tile,part.offset..part.offset+part.length,self.block_size,"validatedTlmSot");
                    parts.push(part);
                }
                offset += length as u64;
            }
            span.record("discovered_parts", parts.len());
            span.record("range_requests", context.stats.requests - before.requests);
            span.record("range_bytes", context.stats.bytes - before.bytes);
            return Ok(parts);
        }
        if !payload.jpip.is_empty() {
            match self.jpip_selected_parts(&payload.jpip,selected,selected_prefix,complete_parts,context) {
                Ok(parts)=>{span.record("index_source","jpip-tpix");crate::source_admission::index_finished();return Ok(parts);}
                Err(error)=>{context.check()?;tracing::info!(context=context.trace_id,optimization="embedded_tile_part_lookup",reason=%error,action="bounded_discovery","Optional JPIP index rejected by source validation");}
            }
        }
        if let Some(parts)=index_cache::ready(&self.id,header,self.meta.size,selected) {
            span.record("index_source","published-prefix");
            tracing::info!(context=context.trace_id,optimization="learned_tile_part_index",index_source="published_prefix",selected_parts=parts.len(),action="reuse_validated_offsets","Reusing learned tile-part offsets");
            crate::source_admission::index_finished();
            return Ok(parts);
        }
        let mut index = lock_cancellable(&self.parts, context)?;
        let mut backed = false;
        if index.parts.is_empty() {
            if let Some(saved) = index_cache::load(&self.id, header, self.meta.size) {
                tracing::info!(context=context.trace_id,optimization="learned_tile_part_index",index_source="metadata_cache",parts=saved.parts.len(),complete=saved.complete,action="reuse_validated_offsets","Reusing learned tile-part offsets");
                *index = saved; backed = true;
            }
        }
        let retain_discovery_prefix=self.cache.limit<8*1024*1024 && whole_view
            && header.prefix_compatible && header.layers==1
            && selected.len() as u64==u64::from(header.layout.columns)*u64::from(header.layout.rows)
            && discovery_prefix_fits(selected.len() as u64,scan_ahead,self.block_size,
                context.pinned.values().map(|b|b.len()).sum());
        let previous_count = index.parts.len();
        if payload.tlm.is_empty() && previous_count==0 {
            tracing::info!(context=context.trace_id,optimization="direct_tile_part_lookup",reason="no_usable_tlm",action="discover_sot_offsets", "File index optimization unavailable; using bounded discovery");
        }
        let anonymous_tlm = payload.tlm.iter().any(|(tile,_)| tile.is_none());
        span.record("index_source", if anonymous_tlm { "tlm-offsets" } else if backed { "metadata-cache" } else if previous_count > 0 { "retained" } else { "cold-sot" });
        let mut speculative=lock_cancellable(&self.speculative_index,context)?;
        let index_shards::State {attempted,lanes}=&mut *speculative;
        let mut prefetched_until = 0;
        let result = (|| -> Result<Vec<TilePart>> {
        let mut counts = HashMap::<u16, (u8, usize)>::new();
        for part in &index.parts {
            let count = counts.entry(part.tile).or_insert((part.total, 0)); count.1 += 1;
        }
        let mut pending = selected.iter().copied().filter(|tile|
            counts.get(tile).is_none_or(|&(total, count)| total == 0 || count != usize::from(total))).collect::<HashSet<_>>();
        while !index.complete && !pending.is_empty() {
            context.check()?;
            // First-connection setup inflates the opening GET. Calibrate with
            // required SOT reads before deciding on speculative bulk traffic.
            // A persisted/cached prefix must also observe a real preparation GET.
            if !anonymous_tlm && !*attempted && index.parts.len()>=2 && context.stats.requests>0 {
                match index_shards::calibration(u64::from(header.layout.columns)*u64::from(header.layout.rows),context.stats.requests,self.minimum_range_latency_us()) {
                    index_shards::Calibration::Fast=>{*attempted=true;},
                    index_shards::Calibration::Waiting=>{},
                    index_shards::Calibration::Ready=>{if let Some(found)=self.index_lanes(&index,selected,whole_view,attempted,context)? {*lanes=found;}},
                }
            }
            if let Some(parts)=index_shards::connected(lanes,index.cursor) {
                for part in parts {
                    ensure!(part.offset==index.cursor,"disconnected speculative tile-part chain");
                    ensure!(index.parts.len()<100_000,"too many JPEG 2000 tile parts");
                    read_trace::index_entry_from(context.trace_id,&self.id,index.parts.len(),part.tile,part.offset..part.offset+part.length,self.block_size,"validatedConnectedSot");
                    let count=counts.entry(part.tile).or_insert((part.total,0));count.1+=1;
                    if count.0>0 && count.1==usize::from(count.0) {pending.remove(&part.tile);}
                    index.cursor+=part.length;index.parts.push(part);
                }
                index_shards::prune_covered(lanes,index.cursor);
                index_cache::publish(&self.id,&index,header,self.meta.size);
                #[cfg(all(feature="grok",unix))] self.pipeline_ready(&index.parts,context)?;
                continue;
            }
            let position = index.parts.len();
            context.trace_index=Some(position);
            let cached_sot = (index.cursor/self.block_size..index.cursor.saturating_add(12).div_ceil(self.block_size))
                .all(|block| { let offset = block*self.block_size;
                    context.pinned.contains_key(&offset) || context.recent.contains(self.block_size,offset) || self.cache.get(&self.cache_key,self.block_size,offset).is_some() });
            if anonymous_tlm && !cached_sot && position >= prefetched_until && position < tlm_offsets.len() {
                let end = (position+16).min(tlm_offsets.len());
                let spans = tlm_offsets[position..end].iter().map(|&offset| (true,offset..offset+12)).collect::<Vec<_>>();
                let leases = self.prefetch_ranges(&spans,context,usize::MAX)?;
                context.pin_packet_blocks(leases);
                prefetched_until = end;
            }
            if !cached_sot && (!lanes.is_empty() || (*attempted && primary_chunks && self.deep_index_depth(whole_view,context)>1)) {self.advance_index_lanes(lanes,index.cursor,scan_ahead,&pending,selected.len() as u64==u64::from(header.layout.columns)*u64::from(header.layout.rows),context)?;if lanes.iter().any(|lane|lane.parts.iter().any(|part|part.offset==index.cursor)) {continue;}}
            // A prior probe can already contain another small tile's SOT and
            // its complete low-resolution payload. Do not extend that cached
            // prefix just to discover the next offset; missing packet bytes
            // are fetched in the subsequent bounded prefix batch.
            let probe_size=if cached_sot {2} else {scan_ahead.max(12)};
            let probe_end = index.cursor.saturating_add(probe_size).min(header.codestream_end);
            let probe = self.read(index.cursor..probe_end, context)?;
            if probe.get(..2) == Some(&[0xff,0xd9]) { index.complete = true; break; }
            ensure!(index.parts.len() < 100_000, "too many JPEG 2000 tile parts");
            let part = self.part_at(index.cursor, context)?;
            if retain_discovery_prefix && part.ordinal==0 && part.total==1 {
                context.pin_recent_range(self.block_size,index.cursor..probe_end);
            }
            context.trace_index=None;
            read_trace::index_entry(context.trace_id,&self.id,position,part.tile,part.offset..part.offset+part.length,self.block_size);
            if anonymous_tlm && position < payload.tlm.len() {
                let (tile,length) = payload.tlm[position];
                ensure!(tlm_offsets[position] == part.offset && u64::from(length) == part.length
                    && tile.is_none_or(|tile| tile == part.tile),"TLM does not match SOT");
            }
            let count = counts.entry(part.tile).or_insert((part.total, 0)); count.1 += 1;
            if count.0 > 0 && count.1 == usize::from(count.0) { pending.remove(&part.tile); }
            index.cursor += part.length as u64;
            index.parts.push(part);
            if index.parts.len()==1 || index.parts.len()%16==0 {
                index_cache::publish(&self.id,&index,header,self.meta.size);
                #[cfg(all(feature="grok",unix))] self.pipeline_ready(&index.parts,context)?;
            }
        }
        Ok(index.parts.iter().filter(|p| selected.contains(&p.tile)).cloned().collect())
        })();
        // Preserve discovered offsets even on cancellation/budget exhaustion,
        // but do not retain a growing Vec for every browsed object forever.
        if index.parts.len() > previous_count || (!backed && !index.parts.is_empty()) {
            match index_cache::save(&self.id, &index, header, self.meta.size, result.is_err() || index.cursor.saturating_add(2)>=header.codestream_end) {
                Ok(saved) => backed = saved,
                Err(error) => tracing::debug!(%error, "S3 technical index cache skipped"),
            }
        }
        if index.complete {lanes.clear();} else {index_shards::trim_retained(lanes);}
        let retained = std::env::var("RIVER_S3_RETAINED_INDEX_BYTES").ok()
            .and_then(|value| value.parse::<usize>().ok()).unwrap_or(0).min(1024 * 1024);
        // Without disk backing, preserve incremental discovery up to 1 MiB.
        span.record("discovered_parts", index.parts.len().saturating_sub(previous_count));
        span.record("range_requests", context.stats.requests - before.requests);
        span.record("range_bytes", context.stats.bytes - before.bytes);
        trim_retained_index(&mut index, header.first_part, if backed { retained } else { 1024 * 1024 });
        drop(index);
        context.trace_index=None;
        if result.is_ok() {crate::source_admission::index_finished();}
        result
    }
    fn part_at(&self, offset: u64, context: &mut ReadContext) -> Result<TilePart> {
        context.check()?;
        if let Some(part)=context.validated.get(&offset) {return Ok(part.clone());}
        if let Some((part,_))=tile_header_cache::Cache::shared().indexed(&self.id,offset) {return Ok(part);}
        let end = offset.checked_add(12).context("SOT probe offset overflow")?;
        ensure!(end <= self.header.codestream_end, "truncated JPEG 2000 SOT marker");
        let bytes = self.read(offset..end, context)?;
        ensure!(bytes[..4] == [0xff, 0x90, 0, 10], "invalid JPEG 2000 SOT marker");
        let declared = be32(&bytes, 6)?;
        if declared == 0 {
            let end = self.header.codestream_end;
            ensure!(end >= offset + 16 && self.read(end - 2..end, context)? == [0xff, 0xd9],
                "unspecified final tile-part length requires a bounded codestream ending in EOC");
        }
        let length = resolved_part_length(declared, offset, self.header.codestream_end)?;
        let part=TilePart { tile: be16(&bytes, 4)?, offset, length, ordinal: bytes[10], total: bytes[11] };
        if context.validated.len()<4096 && context.admit_metadata(std::mem::size_of::<TilePart>()+64) {context.validated.insert(offset,part.clone());}
        Ok(part)
    }

    /// Returns a valid compressed subset in the shared decoder filesystem.
    /// Original packet bytes are retained; unused packets become empty packets.
    pub fn prepare_gpu_preview(&self,reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
        // Keep the original low-resolution entropy, with bounded geometry.
        let payload=self.acquire_render_header(context)?;
        self.prepare_single_prefix(&payload,reduction,layers,context)
    }
    pub fn prepare(&self, tiles: &[(u32, u32)], source_tile: u32, reduction: u8, layers: u32, whole_view: bool, complete_grid: bool,
        context: &mut ReadContext) -> Result<PreparedSubset> {
        let _source_preparation=self.read_profile.preparation();
        let mut total=SourceProfile::new(context);
        let result=self.prepare_inner(tiles,source_tile,reduction,layers,whole_view,complete_grid,context);
        profile_stage("total",context,&mut total);
        result
    }
    fn prepare_inner(&self, tiles: &[(u32, u32)], source_tile: u32, reduction: u8, layers: u32, whole_view: bool, complete_grid: bool,
        context: &mut ReadContext) -> Result<PreparedSubset> {
        let h = &self.header;
        let mut profile = SourceProfile::new(context);
        ensure!(reduction < h.levels, "unsupported remote resolution");
        ensure!(!complete_grid || h.image_origin == (0,0), "GPU range subset needs a zero-origin reference grid");
        let payload = self.acquire_render_header(context)?;
        let selected=self.native_selection(tiles,source_tile)?;
        if !complete_grid {
            if whole_view {if let Some(prepared)=self.prepare_sequential_plt_prefix(&payload,reduction,layers,context)? {return Ok(prepared);}}
            if let Some(prepared)=self.prepare_index_only_prefix(&payload,reduction,layers,context)? {return Ok(prepared);}
            if let Some(prepared)=self.prepare_single_prefix(&payload,reduction,layers,context)? {return Ok(prepared);}
        }
        let roi=precinct_roi::Selection::new(h,tiles,source_tile,reduction,context).filter(|roi| !selected.iter().all(|&tile|roi.covers(h.tile_bounds(tile))));
        if !complete_grid {if let Some(roi)=roi.as_ref() {if let Some(prepared)=self.prepare_streamed_single_roi(&payload,reduction,layers,roi,context)? {return Ok(prepared);}}}
        if !complete_grid && roi.is_none() {if let Some(prepared)=self.prepare_multipart_prefix(&selected,&payload,reduction,layers,context)? {return Ok(prepared);}}
        if !complete_grid && roi.is_none() {if let Some(prepared)=self.prepare_unindexed_coarse_prefix(&selected,&payload,reduction,layers,context)? {return Ok(prepared);}}
        let rebase=if !complete_grid {roi.as_ref().and_then(|roi|roi.rebase(h))}else{None};
        #[cfg(all(feature="grok",unix))]
        if context.pipeline_requested && rebase.is_none() {
            context.pipeline_requested=false;
            if 1u32.checked_shl(u32::from(reduction)).is_some_and(|scale|source_tile%scale==0) {context.pipeline=pipeline::Pipeline::start(self,reduction,layers,&selected,context.cancel.clone());}
        }
        if complete_grid && whole_view && self.meta.size <= env_limit("RIVER_S3_SMALL_OBJECT_BYTES", 8 * 1024 * 1024) as u64
            && !(h.layout.width.is_power_of_two() && h.layout.height.is_power_of_two()) {
            let spans = vec![(true, 0..self.meta.size)];
            let leases = self.prefetch_packets(&spans, context)?;
            context.pin_small_object(leases);
            let bytes = self.read(0..self.meta.size, context)?;
            return Ok(PreparedSubset {view_reduction:0,technical_origin:None,bytes, route: "s3-required-object-ranges", jp2: !payload.container.is_empty(),
                width: h.width, height: h.height, layout: h.layout, offset_x: 0, offset_y: 0 });
        }
        let x_min = selected.iter().map(|&tile| u32::from(tile) % h.layout.columns).min().unwrap();
        let x_max = selected.iter().map(|&tile| u32::from(tile) % h.layout.columns).max().unwrap();
        let y_min = selected.iter().map(|&tile| u32::from(tile) / h.layout.columns).min().unwrap();
        let y_max = selected.iter().map(|&tile| u32::from(tile) / h.layout.columns).max().unwrap();
        let crop = complete_grid && selected.len() as u64 != u64::from(h.layout.columns) * u64::from(h.layout.rows);
        if crop {
            ensure!(h.packet_compatible && h.layout.width.is_power_of_two() && h.layout.height.is_power_of_two()
                && h.layout.width >= 1u32 << (h.levels - 1) && h.layout.height >= 1u32 << (h.levels - 1)
                && selected.len() as u64 == u64::from(x_max - x_min + 1) * u64::from(y_max - y_min + 1),
                "GPU range subset needs a rectangular, power-of-two native tile grid; use OpenJPEG or JP2Grok for this layout");
        }
        let offset_x = if crop { x_min * h.layout.width } else { 0 };
        let offset_y = if crop { y_min * h.layout.height } else { 0 };
        ensure!(offset_x % source_tile == 0 && offset_y % source_tile == 0, "GPU range subset is not aligned to transport tiles");
        let width = if crop { ((x_max + 1) * h.layout.width).min(h.width) - offset_x } else { h.width };
        let height = if crop { ((y_max + 1) * h.layout.height).min(h.height) - offset_y } else { h.height };
        let layout = NativeTileLayoutHint { columns: width.div_ceil(h.layout.width), rows: height.div_ceil(h.layout.height), ..h.layout };
        // Full-resolution, full-quality requests covering every native tile
        // necessarily consume every tile part. Fetch that required codestream
        // before probing individual SOT/tile headers, avoiding serial RTTs.
        // Reduced, quality-limited and partial selections stay demand-driven.
        let full_payload_required = roi.is_none() && reduction == 0 && (layers == 0 || layers >= u32::from(h.layers))
            && selected.len() as u64 == u64::from(h.layout.columns) * u64::from(h.layout.rows)
            && self.meta.size <= env_limit("RIVER_S3_SMALL_OBJECT_BYTES", 8 * 1024 * 1024) as u64;
        if full_payload_required {
            let leases = self.prefetch_packets(&[(true,h.first_part..h.codestream_end)],context)?;
            context.pin_small_object(leases);
        }
        if !full_payload_required && self.meta.size <= env_limit("RIVER_S3_SMALL_OBJECT_BYTES", 8 * 1024 * 1024) as u64
            && (whole_view || selected.len() as u64 == u64::from(h.layout.columns) * u64::from(h.layout.rows)) {
            // A complete view of a small, unindexed/multi-layer object needs
            // every tile part anyway. Batch those required bytes instead of
            // paying one S3 RTT per tile/header. Selection itself remains
            // header-only, and partial views never trigger this path.
            let first = self.part_at(h.first_part, context)?;
            let (_, _, lengths, compatible) = self.tile_header(&first, context)?;
            let selective = h.packet_compatible && compatible && first.ordinal == 0 && first.total <= 1
                && (lengths.is_some() || (h.prefix_compatible && reduction > 0 && (h.layers == 1 || layers == 1)));
            if !selective {
                let spans = vec![(true, 0..self.meta.size)];
                let leases = self.prefetch_packets(&spans, context)?;
                context.pin_small_object(leases);
            }
        }
        // When every native tile is requested, the inline prefix is useful at
        // every discovered SOT. Fold that bounded probe into index discovery
        // instead of paying another RTT per tile. Partial views retain tiny
        // probes so traversal never pulls unselected entropy data.
        let wanted = h.levels-1-reduction;
        let fuse_lowest_prefix = self.meta.size >= 64*1024*1024 && h.prefix_compatible && h.layers==1 && wanted == 0
            && u64::from(h.layout.columns)*u64::from(h.layout.rows) <= 16384;
        let lowest_prefix_probe = if fuse_lowest_prefix {
            env_limit("RIVER_S3_COLD_PREFIX_PROBE_BYTES",2048).clamp(64,4096) as u64
        } else {14};
        let cold_probe = whole_view && fuse_lowest_prefix;
        let learned_prefix_candidate=!complete_grid && roi.is_none() && whole_view && h.layers>1 && reduction.saturating_add(1)==h.levels
            && multipart_prefix::inline_candidate(h) && payload.tlm.is_empty() && payload.jpip.is_empty()
            && std::env::var("RIVER_S3_UNINDEXED_COARSE_PREFIX").is_ok_and(|v|v=="true" || v=="1");
        let scan_ahead = if learned_prefix_candidate {16*1024} else if cold_probe {
            lowest_prefix_probe
        } else if h.prefix_compatible && reduction > 0 && wanted <= 2 && (h.layers == 1 || layers == 1)
            && selected.len() as u64 == u64::from(h.layout.columns)*u64::from(h.layout.rows) {
            prefix_read_ahead(wanted)+64
        } else { 2 };
        let mut retain_native_parts=roi.is_none() && h.packet_compatible && reduction==0 && (layers==0 || layers>=u32::from(h.layers));
        let native=u64::from(h.layout.columns)*u64::from(h.layout.rows);
        let payload_base=context.requests_limit;
        let allowance=context.begin_index_ranges(native);
        let parts = self.selected_parts(&selected, &payload, scan_ahead, lowest_prefix_probe, retain_native_parts, whole_view, context);
        let index_credit=context.finish_index_ranges(allowance);
        let parts=parts?;
        profile_stage("index",context,&mut profile);
        ensure!(selected.iter().all(|tile| parts.iter().any(|p| &p.tile == tile)), "missing requested JPEG 2000 tile parts");
        if learned_prefix_candidate {
            if let Some(prepared)=self.prepare_discovered_multipart_prefix(&selected,&payload,&parts,reduction,layers,context)? {return Ok(prepared);}
        }
        #[cfg(all(feature="grok",unix))] {
            self.pipeline_final(&parts,context,payload_base,index_credit)?;
            if context.pipeline.as_ref().is_some_and(pipeline::Pipeline::complete) {
                return Ok(PreparedSubset {view_reduction:0,technical_origin:None,bytes:vec![],jp2:false,route:"s3-packet-ranges",width,height,layout,offset_x,offset_y});
            }
        }
        // A spatial mask cannot skip packets without their lengths. At full
        // resolution/quality, unindexed parts therefore still require their
        // complete payload. Preserve parallel bounded reads in this case,
        // rather than falling through to serial block misses during copying.
        if !retain_native_parts && roi.is_some() && h.packet_compatible && reduction==0
            && (layers==0 || layers>=u32::from(h.layers)) {
            let mut unindexed=true;
            for part in &parts {
                if self.tile_header(part,context)?.2.is_some() {unindexed=false;break;}
            }
            retain_native_parts=unindexed;
        }
        if !complete_grid {if let Some(prepared)=self.prepare_multipart(&parts,&payload,reduction,layers,roi.as_ref(),context)? {return Ok(prepared);}}
        if let Some(window)=rebase {
            if parts.len()==1 && parts[0].ordinal==0 && parts[0].total<=1 {
                let part=&parts[0];
                let (tile_header,start,lengths,compatible)=self.tile_header(part,context)?;
                if compatible {if let Some(lengths)=lengths {
                    let expected=tile_packet_count(h,part.tile).context("packet count overflow")?;
                    ensure!(expected<=MAX_PACKETS as u64 && expected==lengths.len() as u64,"PLT packet count does not match codestream geometry");
                    let mut offset=start;let mut spans=vec![];
                    self.visit_packets(part.tile,context,|packet,ordinal| {
                        let length=lengths[ordinal];
                        ensure!(length>0,"invalid zero-length PLT packet");
                        let end=offset.checked_add(length).context("PLT packet length overflow")?;
                        ensure!(end<=part.offset+part.length,"PLT packet exceeds tile part");
                        // Keep the window's packet order, but do not fetch
                        // entropy outside the already-validated ROI support.
                        // Empty packets keep its precinct-local state valid.
                        if window.contains(packet) {spans.push((packet.resolution<=h.levels-1-reduction && roi.as_ref().is_none_or(|roi|roi.keeps(h,packet)),offset..end));}
                        offset=end;Ok(())
                    })?;
                    ensure!(offset==part.offset+part.length,"PLT lengths do not cover the tile payload");
                    let selected_payload=self.selected_payload(&spans,context)?;
                    let mut main=payload.main.clone();window.patch_siz(&mut main)?;
                    let mut sot=self.read(part.offset..part.offset+12,context)?;
                    let size=12+tile_header.len()+2+selected_payload.len();
                    sot[6..10].copy_from_slice(&u32::try_from(size)?.to_be_bytes());
                    main.extend(sot);main.extend(tile_header);main.extend([0xff,0x93]);main.extend(selected_payload);main.extend([0xff,0xd9]);
                    ensure!(main.len()<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"compressed subset exceeds request memory budget");
                    let jp2=!payload.container.is_empty();
                    let bytes=if jp2 {
                        let mut container=payload.container.clone();resize_jp2(&mut container,window.width(),window.height())?;
                        container.extend(u32::try_from(main.len()+8)?.to_be_bytes());container.extend(b"jp2c");container.extend(main);container
                    }else{main};
                    return Ok(PreparedSubset {view_reduction:0,technical_origin:Some((window.x0,window.y0)),bytes,jp2,route:"s3-precinct-window-ranges",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0});
                }}
            }
        }
        // A full-resolution/all-quality tile needs every original packet.
        // Copy its selected native parts directly: PLT parsing, packet-order
        // construction and empty-packet reconstruction cannot save any bytes.
        if retain_native_parts {
            let spans=parts.iter().map(|part|(true,part.offset..part.offset+part.length)).collect::<Vec<_>>();
            if context.can_pin_ranges(self.block_size,&spans) {
                let leases=self.prefetch_ranges(&spans,context,usize::MAX)?;
                context.pin_packet_blocks(leases);
            }
        } else {
        // Persisted/TLM offsets let cold processes overlap tile-header reads.
        // A fresh sequential traversal has already cached these blocks.
        let inline_header_probe=(h.prefix_compatible && reduction>0 && wanted<=2
            && (h.layers==1 || layers==1)
            && selected.len() as u64==u64::from(h.layout.columns)*u64::from(h.layout.rows))
            .then_some(scan_ahead);
        let mut header_spans: Vec<_> = parts.iter().map(|part| {
            let cached_sot = (part.offset/self.block_size..part.offset.saturating_add(14).div_ceil(self.block_size))
                .all(|block| {let offset=block*self.block_size;
                    context.pinned.contains_key(&offset) || context.recent.contains(self.block_size,offset) || self.cache.get(&self.cache_key,self.block_size,offset).is_some()});
            // A persisted index removes SOT traversal, but a header-only probe
            // would then require another RTT for nearly every LL packet body.
            // Match the bounded cold-walk prefix for supported single-part views,
            // including intermediate reductions, when the header is uncached;
            // never extend a prefix already obtained during index discovery.
            let bytes=known_header_probe(cached_sot,fuse_lowest_prefix.then_some(lowest_prefix_probe),inline_header_probe,part);
            (true,part.offset..part.offset.saturating_add(bytes).min(part.offset+part.length))
        }).collect();
        // A fused batch must remain readable even with the shared cache off.
        // If it cannot fit the existing request-local pin budget, retain the
        // old header-only plan rather than fetching bytes that would be lost.
        if inline_header_probe.is_some() && !context.can_pin_ranges(self.block_size,&header_spans) {
            header_spans=parts.iter().map(|part| {
                let cached_sot=(part.offset/self.block_size..part.offset.saturating_add(14).div_ceil(self.block_size))
                    .all(|block| {let offset=block*self.block_size;
                        context.pinned.contains_key(&offset) || context.recent.contains(self.block_size,offset) || self.cache.get(&self.cache_key,self.block_size,offset).is_some()});
                let bytes=known_header_probe(cached_sot,fuse_lowest_prefix.then_some(lowest_prefix_probe),None,part);
                (true,part.offset..part.offset.saturating_add(bytes).min(part.offset+part.length))
            }).collect();
        }
        let header_leases = self.prefetch_ranges(&header_spans, context, usize::MAX)?;
        // Keep these bounded request-local blocks readable even if another
        // viewer evicts them from the shared compressed-byte cache.
        context.pin_packet_blocks(header_leases);
        context.allow_header_ranges(parts.len() as u64,payload_base,index_credit);
        self.prefetch_tile_headers(&parts,if fuse_lowest_prefix {lowest_prefix_probe}else{4},context)?;
        // Headers prove a lower bound on the resulting compressed subset.
        // If complete payloads alone exceed its memory ceiling, consuming the
        // read budget cannot produce a usable subset. Fall back before reading
        // entropy data. PLT and supported prefix layouts remain selective.
        let mut required_full_payload = 0u64;
        let mut all_selective=true;
        for part in &parts {
            let (_, start, lengths, compatible) = self.tile_header(part,context)?;
            let selective = h.packet_compatible && compatible && part.ordinal == 0 && part.total <= 1
                && (lengths.is_some() || (h.prefix_compatible && reduction > 0 && (h.layers == 1 || layers == 1)));
            if !selective {
                all_selective=false;
                required_full_payload = required_full_payload.checked_add(part.offset+part.length-start)
                    .context("compressed subset size overflow")?;
                ensure!(required_full_payload <= context.limit.saturating_add((MAX_HEADER+MAX_PACKETS) as u64),
                    "S3 compressed-read budget exceeded; complete tile payloads exceed subset memory budget");
            }
        }
        if all_selective && h.prefix_compatible && reduction>0 && (h.layers==1 || layers==1) {
            context.allow_prefix_ranges(parts.len() as u64,payload_base,index_credit);
        }
        // Once lazy SOT traversal has found the requested tiles, fetch their
        // small resolution-prefix probes together. Keep distant ranges separate:
        // merging them would download unrelated high-resolution entropy data.
        // This overlaps S3 RTTs without reading any unselected tile payload.
        let mut prefix_spans = vec![];
        let mut lowest_prefixes = vec![];
        if h.prefix_compatible && reduction > 0 && (h.layers == 1 || layers == 1) {
            let read_ahead = prefix_read_ahead(h.levels - 1 - reduction);
            for part in &parts {
                if part.ordinal != 0 || part.total > 1 { continue; }
                let (_, start, lengths, compatible) = self.tile_header(part, context)?;
                if compatible && lengths.is_none() {
                    prefix_spans.push((true, start..(start + read_ahead).min(part.offset + u64::from(part.length))));
                    if h.levels - 1 - reduction == 0 { lowest_prefixes.push((part.clone(),start)); }
                }
            }
        }
        let prefix_leases = self.prefetch_ranges(&prefix_spans, context, usize::MAX)?;
        if self.cache.limit<8*1024*1024 {context.pin_packet_blocks(prefix_leases);}
        profile_stage("headers",context,&mut profile);
        self.prefetch_lowest_prefixes(&lowest_prefixes, context)?;
        profile_stage("prefix",context,&mut profile);
        self.prefetch_plt_packets(&parts,reduction,layers,roi.as_ref(),context)?;
        profile_stage("plt",context,&mut profile);
        }
        #[cfg(all(feature="grok",unix))] self.pipeline_final(&parts,context,payload_base,index_credit)?;
        if let Some(view)=self.prepare_reduced_prefix_view(&parts,&payload,reduction,whole_view,complete_grid,context)? {
            profile_stage("reduced-view",context,&mut profile);return Ok(view);
        }
        let mut codestream = payload.main.clone();
        if crop { resize_siz(&mut codestream, width, height)?; }
        let mut packet_subset = true;
        for part in &parts {
            context.check()?;
            if retain_native_parts {
                ensure!(self.part_at(part.offset,context)?==*part,"cached tile-part index disagrees with source SOT");
                let mut bytes=self.read(part.offset..part.offset+part.length,context)?;
                if crop {
                    let tile=(u32::from(part.tile)/h.layout.columns-y_min)*layout.columns
                        +u32::from(part.tile)%h.layout.columns-x_min;
                    bytes[4..6].copy_from_slice(&u16::try_from(tile)?.to_be_bytes());
                }
                // An originally unsized final part now has an explicit extent.
                bytes[6..10].copy_from_slice(&u32::try_from(part.length)?.to_be_bytes());
                codestream.extend(bytes);packet_subset=false;
                ensure!(codestream.len()<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"compressed subset exceeds request memory budget");
                continue;
            }
            let (tile_header, payload_start, lengths, compatible) = self.tile_header(part, context)?;
            let selected_payload = if h.packet_compatible && compatible && part.ordinal == 0 && part.total <= 1 {
                if let Some(lengths) = lengths {
                    ensure!(tile_packet_count(h,part.tile)==Some(lengths.len() as u64), "PLT packet count does not match codestream geometry");
                    let mut offset = payload_start;
                    let mut spans: Vec<(bool, Range<u64>)> = vec![];
                    self.visit_packets(part.tile,context,|packet,ordinal| {
                        let length=lengths[ordinal];
                        ensure!(length > 0, "invalid zero-length PLT packet");
                        let keep = packet.resolution <= h.levels - 1 - reduction && (layers == 0 || u32::from(packet.layer) < layers)
                            && roi.as_ref().is_none_or(|roi|roi.keeps(h,packet));
                        let end = offset.checked_add(length).context("PLT packet length overflow")?;
                        ensure!(end <= part.offset + part.length as u64, "PLT packet exceeds tile part");
                        spans.push((keep, offset..end)); offset = end;Ok(())
                    })?;
                    ensure!(offset == part.offset + part.length as u64, "PLT lengths do not cover the tile payload");
                    self.selected_payload(&spans, context)?
                } else if h.prefix_compatible && reduction > 0 && part.total <= 1 && (h.layers == 1 || layers == 1) {
                    let (packets, total) = self.ordered_packets(part.tile, Some(h.levels - 1 - reduction), context)?;
                    if let Some(bytes) = self.prefix_payload(part, payload_start, &packets, total, reduction, roi.as_ref(),context)? { bytes }
                    else { packet_subset = false; self.read(payload_start..part.offset + part.length as u64, context)? }
                } else {
                    packet_subset = false;
                    self.read(payload_start..part.offset + part.length as u64, context)?
                }
            } else {
                packet_subset = false;
                self.read(payload_start..part.offset + part.length as u64, context)?
            };
            let mut sot = self.read(part.offset..part.offset + 12, context)?;
            if crop {
                let tile = (u32::from(part.tile) / h.layout.columns - y_min) * layout.columns
                    + u32::from(part.tile) % h.layout.columns - x_min;
                sot[4..6].copy_from_slice(&u16::try_from(tile)?.to_be_bytes());
            }
            let size = 12 + tile_header.len() + 2 + selected_payload.len();
            sot[6..10].copy_from_slice(&u32::try_from(size)?.to_be_bytes());
            codestream.extend(sot); codestream.extend(tile_header);
            codestream.extend([0xff, 0x93]); codestream.extend(selected_payload);
            ensure!(codestream.len() <= context.limit as usize + MAX_HEADER + MAX_PACKETS, "compressed subset exceeds request memory budget");
        }
        #[cfg(all(feature="grok",unix))] self.pipeline_final(&parts,context,payload_base,index_credit)?;
        codestream.extend([0xff, 0xd9]);
        let jp2 = !payload.container.is_empty();
        let bytes = if jp2 {
            let mut container = payload.container.clone();
            if crop { resize_jp2(&mut container, width, height)?; }
            let mut bytes = Vec::with_capacity(container.len() + 8 + codestream.len());
            bytes.extend_from_slice(&container);
            bytes.extend_from_slice(&u32::try_from(codestream.len() + 8)?.to_be_bytes());
            bytes.extend_from_slice(b"jp2c");
            bytes.extend(codestream);
            bytes
        } else { codestream };
        Ok(PreparedSubset {view_reduction:0,technical_origin:None,bytes, jp2, route: if packet_subset { "s3-packet-ranges" } else { "s3-tile-part-ranges" },
            width, height, layout, offset_x, offset_y })
    }
    fn prefetch_tile_headers(&self,parts:&[TilePart],prefix_probe:u64,context:&mut ReadContext)->Result<()> {
        if parts.len()<2 {return Ok(());}
        // Keep marker waves bounded without serializing every header when a
        // large preview touches more than4096 native tile parts.
        for chunk in parts.chunks(256) { self.prefetch_tile_header_chunk(chunk,prefix_probe,context)?; }
        Ok(())
    }
    fn prefetch_tile_header_chunk(&self,parts:&[TilePart],prefix_probe:u64,context:&mut ReadContext)->Result<()> {
        let mut active=Vec::with_capacity(parts.len());
        for part in parts {
            context.check()?;
            // Parsed metadata is independently version/extent pinned. Do not
            // walk its markers again just to warm an optional byte cache.
            if self.cached_tile_header(part,context).is_none() {active.push((part,part.offset+12,0usize));}
        }
        while !active.is_empty() {
            let mut spans=Vec::with_capacity(active.len());let mut next=Vec::with_capacity(active.len());
            for (part,cursor,markers) in active {
                context.check()?;
                ensure!(markers<4096,"too many JPEG 2000 tile-header markers");
                let prefix=self.read(cursor..(cursor+4).min(part.offset+part.length),context)?;
                let code=be16(&prefix,0)?;
                if code==0xff93 {continue;}
                let length=u64::from(be16(&prefix,2)?);
                let end=cursor.checked_add(2).and_then(|n|n.checked_add(length)).context("tile-header marker extent overflow")?;
                ensure!(length>=2 && end<=part.offset+part.length,"invalid tile-header marker extent");
                if code==0xff64 {
                    ensure!(length>=4,"truncated JPEG 2000 tile comment marker");
                    // Comments are skipped, including during parallel discovery.
                    // A displaced SOD should retain the same bounded LL
                    // lookahead used at an ordinary SOT, rather than forcing
                    // dependent packet-header RTTs after a long comment.
                    spans.push((true,end..end.saturating_add(prefix_probe).min(part.offset+part.length)));
                } else {
                    ensure!(cursor-part.offset+length<=MAX_HEADER as u64,"invalid or oversized tile header");
                    spans.push((true,cursor..(end+4).min(part.offset+part.length)));
                }
                next.push((part,end,markers+1));
            }
            if next.is_empty() {break;}
            // Bound request-local metadata retention; oversized plans keep the
            // existing per-part reader instead of fetching data twice.
            if !context.can_pin_ranges(self.block_size,&spans) && (self.cache.limit as u64)<context.limit {return Ok(());}
            let leases=self.prefetch_ranges(&spans,context,usize::MAX)?;
            context.pin_packet_blocks(leases);active=next;
        }
        Ok(())
    }
    #[track_caller]
    fn read_tile_header_bytes(&self,range:Range<u64>,ahead:&Option<(u64,Vec<u8>)>,context:&mut ReadContext)->Result<Vec<u8>> {
        context.check()?;
        let mut bytes=header_window_prefix(range.clone(),ahead);
        context.stats.cache_bytes+=bytes.len() as u64;
        let missing=range.start+bytes.len() as u64;
        if missing<range.end {bytes.extend(self.read(missing..range.end,context)?);}
        Ok(bytes)
    }
    fn cached_tile_header(&self,part:&TilePart,context:&mut ReadContext)->Option<ParsedTileHeader> {
        // This request-local header has already validated this exact SOT
        // against the version-pinned source. Reusing it need not reread SOT.
        if let Some((validated,header))=context.tile_headers.get(&part.offset) {
            if validated==part {return Some(header.clone());}
        }
        let header=tile_header_cache::Cache::shared().get(&self.id,part)?;
        let bytes=header.0.capacity()+header.2.as_ref().map_or(0,|v|v.capacity()*8)+128+std::mem::size_of::<TilePart>();
        if context.tile_headers.len()<16384 && context.admit_metadata(bytes) {
            context.tile_headers.insert(part.offset,(part.clone(),header.clone()));
        }
        Some(header)
    }
    fn tile_header(&self, part: &TilePart, context: &mut ReadContext) -> Result<ParsedTileHeader> {
        context.check()?;
        if let Some(header)=self.cached_tile_header(part,context) {return Ok(header);}
        ensure!(self.part_at(part.offset, context)? == *part, "cached tile-part index disagrees with source SOT");
        let mut cursor = part.offset + 12;
        let mut header = vec![];
        let mut plt = BTreeMap::new();
        let mut compatible = true;
        let mut markers = 0;
        let mut ahead:Option<(u64,Vec<u8>)>=None;
        loop {
            markers += 1;
            ensure!(markers <= 4096, "too many JPEG 2000 tile-header markers");
            let marker = self.read_tile_header_bytes(cursor..cursor + 2,&ahead,context)?;
            let code = be16(&marker, 0)?;
            if code == 0xff93 { cursor += 2; break; }
            let size = self.read_tile_header_bytes(cursor + 2..cursor + 4,&ahead,context)?;
            let length = be16(&size, 0)? as u64;
            ensure!(length >= 2 && cursor + 2 + length <= part.offset + part.length as u64, "invalid tile-header marker extent");
            if code == 0xff64 {
                ensure!(length >= 4, "truncated JPEG 2000 tile comment marker");
                cursor += 2 + length;
                continue;
            }
            ensure!(length >= 2 && cursor + 2 + length <= part.offset + part.length as u64 && cursor - part.offset + length <= MAX_HEADER as u64, "invalid or oversized tile header");
            let required_end=cursor+2+length;
            let policy=crate::read_policy::settings();
            let complete=part.ordinal==0 && (part.total==1 || (part.total==0 && part.offset+part.length==self.header.codestream_end.saturating_sub(2)));
            let packets=if code==0xff58 && complete && compatible && self.header.packet_compatible {tile_packet_count(&self.header,part.tile).unwrap_or(0)}else{0};
            let cached=header_window_prefix(cursor..required_end,&ahead).len() as u64==required_end-cursor;
            let read_end=if cached {required_end}else{plt_read_ahead_end(cursor,required_end,part.offset+part.length,part.offset+MAX_HEADER as u64,
                packets,policy.mode==crate::read_policy::Mode::Adaptive,self.planning_latency_us(),
                u64::from(policy.max_overfetch_ratio).min(crate::read_policy::explicit_overfetch_cap().unwrap_or(16)),context.limit.saturating_sub(context.stats.bytes))};
            let mut bytes=self.read_tile_header_bytes(cursor..read_end,&ahead,context)?;
            let segment=if read_end>required_end {
                let segment=bytes[..(required_end-cursor) as usize].to_vec();ahead=Some((cursor,bytes));segment
            }else{bytes.truncate((required_end-cursor) as usize);bytes};
            if code == 0xff58 {
                ensure!(segment.len() >= 5 && !plt.contains_key(&segment[4]), "invalid PLT sequence");
                plt.insert(segment[4], segment[5..].to_vec());
            } else {
                compatible &= if code == 0xff53 { identical_coc(&segment[4..], self.header.components, &self.header.cod) }
                    else { !matches!(code, 0xff52 | 0xff5f | 0xff61) };
                header.extend(segment);
            }
            cursor += 2 + length;
        }
        let lengths = if compatible && !plt.is_empty() {
            let mut bytes = vec![];
            for (expected, (&number, chunk)) in plt.iter().enumerate() {
                ensure!(expected == number as usize, "missing PLT segment"); bytes.extend(chunk);
            }
            Some(Arc::new(parse_plt(&bytes)?))
        } else { None };
        if lengths.is_none() && compatible && !context.reported_missing_plt {
            context.reported_missing_plt=true;
            tracing::info!(context=context.trace_id,tile=part.tile,optimization="packet_length_lookup",reason="tile_part_has_no_plt",action="try_inline_or_tile_part_reads","File packet index optimization unavailable; using alternate read path");
        }
        let parsed=(header,cursor,lengths,compatible);
        if parsed.2.as_ref().is_some_and(|v|!v.is_empty()) {tile_header_cache::Cache::shared().put(&self.id,part,&parsed);}
        let bytes=parsed.0.capacity()+parsed.2.as_ref().map_or(0,|v|v.capacity()*8)+128+std::mem::size_of::<TilePart>();
        if context.tile_headers.len()<16384 && context.admit_metadata(bytes) {context.tile_headers.insert(part.offset,(part.clone(),parsed.clone()));}
        Ok(parsed)
    }
    /// Overlap selected PLT packets across native tile parts, rather than
    /// waiting for each part's payload before starting the following one.
    fn prefetch_plt_packets(&self, parts: &[TilePart], reduction: u8, layers: u32, roi:Option<&precinct_roi::Selection>, context: &mut ReadContext) -> Result<()> {
        if !self.header.packet_compatible || parts.len() < 2 || parts.len() > 4096 { return Ok(()); }
        let mut spans = vec![];
        let mut bytes = 0u64;
        let mut blocks = BTreeSet::new();
        for part in parts {
            if part.ordinal != 0 || part.total > 1 { continue; }
            let (_,mut cursor,lengths,compatible) = self.tile_header(part,context)?;
            let Some(lengths) = lengths.filter(|_| compatible) else { continue; };
            ensure!(tile_packet_count(&self.header,part.tile)==Some(lengths.len() as u64),"PLT packet count does not match codestream geometry");
            #[derive(Debug)]struct PrefetchFull;
            impl std::fmt::Display for PrefetchFull {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("bounded PLT prefetch full")}}
            impl std::error::Error for PrefetchFull {}
            let traversal=self.visit_packets(part.tile,context,|packet,ordinal| {
                let length=lengths[ordinal];
                ensure!(length > 0,"invalid zero-length PLT packet");
                let end = cursor.checked_add(length).context("PLT packet length overflow")?;
                ensure!(end <= part.offset+part.length,"PLT packet exceeds tile part");
                if packet.resolution <= self.header.levels-1-reduction && (layers==0 || u32::from(packet.layer)<layers) && roi.is_none_or(|roi|roi.keeps(&self.header,packet)) {
                    bytes = bytes.saturating_add(length);
                    if spans.len() >= 16384 || bytes > 2*1024*1024 { return Err(PrefetchFull.into()); }
                    for block in cursor/self.block_size..end.div_ceil(self.block_size) {
                        blocks.insert(block);
                        if blocks.len() as u64*self.block_size > 4*1024*1024 { return Err(PrefetchFull.into()); }
                    }
                    spans.push((true,cursor..end));
                }
                cursor = end;Ok(())
            });
            if traversal.as_ref().is_err_and(|error|error.downcast_ref::<PrefetchFull>().is_some()) {return Ok(());}
            traversal?;
            ensure!(cursor == part.offset+part.length,"PLT lengths do not cover the tile payload");
        }
        let leases = self.prefetch_ranges(&spans,context,48)?;
        let required = leases.into_iter().filter(|(offset,_)| blocks.contains(&(offset/self.block_size))).collect();
        context.pin_packet_blocks(required);
        Ok(())
    }

    fn selected_payload(&self, spans: &[(bool, Range<u64>)], context: &mut ReadContext) -> Result<Vec<u8>> {
        self.selected_payload_retaining(spans,None,context)
    }
    fn selected_payload_retaining(&self,spans:&[(bool,Range<u64>)],retain:Option<Range<u64>>,context:&mut ReadContext)->Result<Vec<u8>> {
        let leases = self.prefetch_packets(spans, context)?;
        if let Some(range)=retain.filter(|r|r.start<r.end) {
            // Share already-fetched prefix blocks: normally a next-header
            // window,or bounded useful payload retained for small-cache fallback.
            let blocks=leases.iter().filter(|(offset,bytes)|**offset<range.end && offset.saturating_add(bytes.len() as u64)>range.start)
                .map(|(&offset,bytes)|(offset,bytes.clone())).collect();
            context.pin_packet_blocks(blocks);
        }
        let fill=1+usize::from(self.header.cod[0]&2!=0)*6+usize::from(self.header.cod[0]&4!=0)*2;
        let capacity=spans.iter().try_fold(0usize,|n,(keep,range)|n.checked_add(if *keep {usize::try_from(range.end-range.start).ok()?}else{fill}))
            .context("prepared packet size overflow")?;
        ensure!(capacity<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"compressed subset exceeds request memory budget");
        let mut result = Vec::with_capacity(capacity);
        for (ordinal,(keep, range)) in spans.iter().enumerate() {
            if *keep {
                let mut cursor = range.start;
                while cursor < range.end {
                    context.check()?;
                    let offset = cursor / self.block_size * self.block_size;
                    let block = leases.get(&offset).context("missing leased packet bytes")?;
                    let within = (cursor - offset) as usize;
                    let count = (range.end - cursor).min(block.len() as u64 - within as u64) as usize;
                    result.extend_from_slice(&block[within..within + count]);
                    cursor += count as u64;
                }
            }
            else { append_empty_packet(&mut result,self.header.cod[0],ordinal); }
        }
        Ok(result)
    }

    fn prefetch_packets(&self, spans: &[(bool, Range<u64>)], context: &mut ReadContext) -> Result<HashMap<u64, Arc<[u8]>>> {
        self.prefetch_ranges(spans, context, 48)
    }

    #[track_caller]
    fn prefetch_execution(&self,ranges:&[Range<u64>],payload:bool)->(usize,bool) {
        let bytes=ranges.iter().map(|r|r.end-r.start).sum();
        let native=u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows);
        let latency=self.planning_latency_us();
        let automatic=std::env::var("RIVER_S3_RANGE_TASKS").unwrap_or_else(|_|"auto".into())=="auto"
            && native>1024 && ranges.len()>=4 && bytes<=16*1024*1024 && latency<2000
            && crate::source_admission::active_preparations()<=1 && self.runtime.metrics().num_workers()>=4;
        let default=if automatic {16}else if payload {payload_range_concurrency(native==1,self.block_size,bytes,ranges,latency)}
            else {range_concurrency(self.block_size,bytes,ranges,latency)};
        (env_limit("RIVER_S3_RANGE_CONCURRENCY",default).clamp(1,16),automatic)
    }
    #[track_caller]
    fn prefetch_ranges(&self, spans: &[(bool, Range<u64>)], context: &mut ReadContext, max_ranges: usize) -> Result<HashMap<u64, Arc<[u8]>>> {
        self.prefetch_ranges_inner(spans,context,max_ranges,true)
    }
    #[track_caller]
    fn prefetch_ranges_inner(&self, spans: &[(bool, Range<u64>)], context: &mut ReadContext, max_ranges: usize, allow_bootstrap:bool) -> Result<HashMap<u64, Arc<[u8]>>> {
        // Cached preview work must not queue behind another index-walk GET.
        context.check()?;
        let mut hits=HashMap::new();
        let mut complete=true;
        'cached: for (_,range) in spans.iter().filter(|(keep,_)|*keep) {
            for block in range.start/self.block_size..range.end.div_ceil(self.block_size) {
                let offset=block*self.block_size;
                if hits.contains_key(&offset) {continue;}
                if let Some(bytes)=context.pinned.get(&offset).cloned().or_else(||context.recent.get(self.block_size,offset)).or_else(||self.cache.get(&self.cache_key,self.block_size,offset)) {hits.insert(offset,bytes);}
                else {complete=false;break 'cached;}
            }
        }
        if complete {context.stats.cache_bytes+=hits.values().map(|v|v.len() as u64).sum::<u64>();return Ok(hits);}
        #[cfg(all(feature="grok",unix))] if context.cache_only {return Err(pipeline::PrefixNotCached.into());}
        let _fetch = self.fetch_admission(context)?;
        let mut blocks = BTreeSet::new();
        let mut leases = HashMap::new();
        for (_, range) in spans.iter().filter(|(keep, _)| *keep) {
            for block in range.start / self.block_size..range.end.div_ceil(self.block_size) {
                let offset = block * self.block_size;
                if leases.contains_key(&offset) || blocks.contains(&offset) { continue; }
                if let Some(bytes) = context.pinned.get(&offset).cloned().or_else(||context.recent.get(self.block_size,offset))
                    .or_else(|| self.cache.get(&self.cache_key, self.block_size, offset)) {
                    context.stats.cache_bytes += bytes.len() as u64;
                    leases.insert(offset, bytes);
                } else { blocks.insert(offset); }
            }
        }
        let settings=crate::read_policy::settings();
        let mut contiguous=crate::read_policy::contiguous_decision(&settings,&self.read_profile.snapshot());
        let mut ranges=contiguous_block_ranges(blocks.iter().copied(),self.block_size,self.meta.size,1024*1024);
        let useful=ranges.iter().map(|r|r.end-r.start).sum();
        let lanes=self.prefetch_execution(&ranges,max_ranges==48).0.min(settings.effective_source_requests()).min(ranges.len()).max(1);
        let contiguous_limit=parallel_contiguous_limit(contiguous.max_range_bytes,useful,lanes,self.block_size);
        if contiguous_limit<contiguous.max_range_bytes {
            contiguous.max_range_bytes=contiguous_limit;
            contiguous.reason="preserveTransferParallelism".into();
        }
        if contiguous_limit>1024*1024 {ranges=contiguous_block_ranges(blocks,self.block_size,self.meta.size,contiguous_limit);}
        self.read_profile.contiguous_decision(contiguous);
        // Bound RTTs without turning an interleaved preview into an almost
        // complete object download. Merge the smallest gaps only when needed.
        let quota = (context.requests_limit - context.stats.requests) as usize;
        ensure!(ranges.is_empty() || quota > 0, "S3 range request limit exceeded");
        let useful_bytes:u64=ranges.iter().map(|r|r.end-r.start).sum();
        let adaptive_prefix=max_ranges==ADAPTIVE_PREFIX_RANGES && crate::read_policy::settings().mode==crate::read_policy::Mode::Adaptive;
        if allow_bootstrap && max_ranges==48 && settings.mode==crate::read_policy::Mode::Adaptive {
            let observation=self.read_profile.snapshot();
            let mature=observation.samples>=u64::from(settings.minimum_samples) && observation.throughput_samples>=3;
            let latency=if mature {self.planning_latency_us()}else{self.minimum_range_latency_us()};
            if latency>=2000 && latency!=u64::MAX && ranges.len()<=quota && context.can_pin_ranges(self.block_size,spans) {
                let refresh=if mature && settings.effective_refresh_payload_costs() {
                    let mut predicted=ranges.clone();
                    adaptive_ranges(&mut predicted,&self.read_profile,quota,context.limit.saturating_sub(context.stats.bytes),useful_bytes,1024*1024,|candidate|self.prefetch_execution(candidate,true).0)?;
                    predicted.len()<ranges.len() && predicted.iter().map(|r|r.end-r.start).sum::<u64>()>useful_bytes
                }else{false};
                if let Some(indices)=payload_bootstrap_indices(&ranges,observation.samples,observation.throughput_samples,settings.minimum_samples,quota,latency,refresh) {
                    let first=indices.into_iter().map(|i|(true,ranges[i].clone())).collect::<Vec<_>>();
                    drop(_fetch);context.pin_packet_blocks(leases);
                    tracing::info!(context=context.trace_id,required_ranges=first.len(),refresh_mature=refresh,action="learn_then_replan","Learning range costs from required payload reads");
                    let fetched=self.prefetch_ranges_inner(&first,context,usize::MAX,false)?;
                    context.pin_packet_blocks(fetched);
                    return self.prefetch_ranges_inner(spans,context,max_ranges,false);
                }
            }
        }
        if allow_bootstrap && adaptive_prefix {
            let settings=crate::read_policy::settings();let observation=self.read_profile.snapshot();
            if let Some(count)=prefix_bootstrap_count(observation.samples,observation.throughput_samples,settings.minimum_samples,ranges.len(),quota) {
                // These are required ranges, not calibration probes. Pin all
                // existing hits and bootstrap results so replanning cannot
                // reread them when the shared cache is disabled or evicted.
                if context.can_pin_ranges(self.block_size,spans) {
                    let first=ranges.iter().take(count).cloned().map(|r|(true,r)).collect::<Vec<_>>();
                    drop(_fetch);context.pin_packet_blocks(leases);
                    if std::env::var_os("RIVER_PROFILE_SOURCE").is_some() {
                        tracing::info!(samples=observation.samples,throughput_samples=observation.throughput_samples,minimum_samples=settings.minimum_samples,required_bootstrap_ranges=count,"fetching useful prefix ranges before learned planning");
                    }
                    let fetched=self.prefetch_ranges_inner(&first,context,usize::MAX,false)?;
                    context.pin_packet_blocks(fetched);
                    return self.prefetch_ranges_inner(spans,context,max_ranges,false);
                }
            }
        }
        let live_required=(max_ranges==48 && settings.mode==crate::read_policy::Mode::Adaptive && ranges.len()<=1024
            && matches!(std::env::var("RIVER_S3_LIVE_PAYLOAD_PLAN").as_deref(),Ok("1"|"true"))).then(||ranges.clone());
        if adaptive_prefix {
            // A learned4x proposal may meet a strict request cap that the
            // legacy2x merge cannot. Evaluate it before the legacy quota error.
            // Prefix batches have no additional48-range payload target, so a
            // quota-valid proposal cannot be amplified again below.
            adaptive_ranges(&mut ranges,&self.read_profile,quota,context.limit.saturating_sub(context.stats.bytes),useful_bytes,PREFIX_MERGE_BYTES,
                |candidate|self.prefetch_execution(candidate,false).0)?;
        }
        merge_ranges(&mut ranges, quota.min(max_ranges), quota, context.limit.saturating_sub(context.stats.bytes),
            env_limit("RIVER_S3_RANGE_OVERFETCH_RATIO", 2).clamp(1, 16) as u64)?;
        if max_ranges==48 || (max_ranges==ADAPTIVE_PREFIX_RANGES && !adaptive_prefix) {
            adaptive_ranges(&mut ranges,&self.read_profile,quota,context.limit.saturating_sub(context.stats.bytes),useful_bytes,
                if max_ranges==ADAPTIVE_PREFIX_RANGES {PREFIX_MERGE_BYTES}else{1024*1024},
                |candidate|self.prefetch_execution(candidate,max_ranges==48).0)?;
        }
        let bytes: u64 = ranges.iter().map(|range| range.end - range.start).sum();
        ensure!(context.stats.bytes + bytes <= context.limit, "S3 compressed-read budget exceeded");
        let mode=std::env::var("RIVER_S3_RANGE_TASKS").unwrap_or_else(|_|"auto".into());
        let (concurrency,automatic)=self.prefetch_execution(&ranges,max_ranges==48);
        let deadline = context.deadline;
        let cancel = context.cancel.clone();
        let range_client=self.range_client(context);
        if let Some(required)=live_required {
            let lanes=concurrency.min(settings.effective_source_requests()).min(ranges.len()).max(1);
            let observation=self.read_profile.snapshot();
            let useful_only=ranges.iter().filter(|r|r.end-r.start>=crate::read_policy::THROUGHPUT_SAMPLE_BYTES && payload_live::required_only(r,&required)).count();
            let has_overfetch=ranges.iter().any(|r|!payload_live::required_only(r,&required));
            let metadata=required.len().saturating_mul(192).saturating_add(ranges.len().saturating_mul(64));
            if has_overfetch && useful_only>=lanes && self.planning_latency_us()>=2000
                && observation.samples>=u64::from(settings.minimum_samples) && observation.body_throughput_samples>=3 && observation.planning_bytes_per_second().is_some()
                && context.admit_metadata(metadata) {
                let result=self.fetch_live_payload(required,ranges,leases,context,lanes,deadline,cancel);
                context.metadata_bytes=context.metadata_bytes.saturating_sub(metadata);
                return result;
            }
        }
        if mode=="spawn" || automatic {
            let client=self.range_client(context);
            self.runtime.block_on(async {
                let mut pending=ranges.iter().cloned();let mut tasks=tokio::task::JoinSet::new();
                let spawn=|tasks:&mut tokio::task::JoinSet<Result<(Range<u64>,Vec<u8>)>>,range:Range<u64>| {
                    let client=client.clone();let cancel=cancel.clone();
                    tasks.spawn(async move {let data=client.fetch(range.clone(),deadline,cancel).await?;Ok((range,data))}.in_current_span());
                };
                for range in pending.by_ref().take(concurrency) {spawn(&mut tasks,range);}
                while let Some(result)=tasks.join_next().await {
                    let (range,data)=result.context("S3 range worker failed")??;context.check()?;
                    context.stats.bytes+=data.len() as u64;context.stats.requests+=1;
                    let blocks=self.cache.put(&self.cache_key,self.block_size,range.start,&data);
                    if self.cache.limit<8*1024*1024 {context.recent.remember(self.block_size,&blocks);}
                    leases.extend(blocks);
                    if let Some(range)=pending.next() {spawn(&mut tasks,range);}
                }
                Ok::<_,anyhow::Error>(())
            })?;
            if max_ranges==ADAPTIVE_PREFIX_RANGES {retain_requested_blocks(&mut leases,spans,self.block_size);}
            return Ok(leases);
        }
        self.runtime.block_on(async {
                let mut pending = futures_util::stream::iter(ranges.iter().cloned().map(|range| {
                    let cancel = cancel.clone();
                    let range_client=range_client.clone();
                    async move {
                    let data = range_client.fetch(range.clone(), deadline, cancel).await?;
                    Ok::<_, anyhow::Error>((range, data))
                }})).buffer_unordered(concurrency);
            while let Some((range, data)) = pending.try_next().await? {
                context.check()?;
                context.stats.bytes += data.len() as u64;
                context.stats.requests += 1;
                let blocks=self.cache.put(&self.cache_key, self.block_size, range.start, &data);
                if self.cache.limit<8*1024*1024 {context.recent.remember(self.block_size,&blocks);}
                leases.extend(blocks);
            }
            Ok::<_, anyhow::Error>(())
        })?;
        if max_ranges==ADAPTIVE_PREFIX_RANGES {retain_requested_blocks(&mut leases,spans,self.block_size);}
        Ok(leases)
    }

    #[track_caller]
    fn range_client(&self,context:&ReadContext)->RangeClient {context.wire_budget.set_limit(context.requests_limit);RangeClient {budget:context.wire_budget.clone(),trace_id:context.trace_id,object_id:self.id.clone(),caller:std::panic::Location::caller(),block_size:self.block_size,part:context.trace_index,store:self.store.clone(),location:self.location.clone(),etag:self.meta.e_tag.clone(),version:self.meta.version.clone(),network_min_us:self.network_min_us.clone(),profile:self.read_profile.clone()}}
    /// Resolve dependent packet headers in rounds across tiles, then overlap
    /// their required bodies. This keeps a tiny LL probe from serializing RTTs.
    fn prefetch_lowest_prefixes(&self, parts: &[(TilePart,u64)], context: &mut ReadContext) -> Result<()> {
        // Request-local leases retain these blocks even when the shared cache
        // is disabled. Low-memory instances can still overlap useful LL reads.
        if parts.is_empty() || parts.len() > 16384 { return Ok(()); }
        struct Prefix { part: TilePart, start: u64, cursor: u64, packets: Vec<Packet>, next: usize, fetched_end:u64 }
        let mut prefixes = Vec::with_capacity(parts.len());
        let mut packet_count = 0;
        for (part,start) in parts {
            let (order,_) = self.ordered_packets(part.tile,Some(0),context)?;
            let mut packets=order.as_ref().clone();
            packets.truncate(packets.iter().take_while(|packet| packet.layer == 0 && packet.resolution == 0).count());
            packet_count += packets.len();
            if packet_count > 65536 { return Ok(()); }
            prefixes.push(Prefix { part: part.clone(), start: *start, cursor: *start, packets, next: 0, fetched_end:*start });
        }
        while prefixes.iter().any(|prefix| prefix.next < prefix.packets.len()) {
            // Earlier packet bodies are already required. Fetch them with the
            // next dependent header rather than paying a separate tiny GET.
            let active=prefixes.iter().filter(|p|p.next<p.packets.len() && p.cursor.saturating_add(64)>p.fetched_end).count().max(1) as u64;
            // Speculation must shrink to the remaining wire-byte allowance;
            // rounding the first/last block can consume two additional blocks.
            let budget=context.limit.saturating_sub(context.stats.bytes)/active;
            let budget=budget.saturating_sub(self.block_size.saturating_mul(2));
            let plans=prefixes.iter().enumerate().filter(|(_,prefix)| prefix.next < prefix.packets.len()
                && prefix.cursor.saturating_add(64)>prefix.fetched_end).map(|(index,prefix)| {
                let ahead=lowest_prefix_window(prefix.cursor-prefix.start,prefix.next,prefix.packets.len(),budget);
                let end=prefix.cursor.saturating_add(ahead).min(prefix.part.offset+prefix.part.length);
                (index,end)
            }).collect::<Vec<_>>();
            let spans=plans.iter().map(|&(index,end)|(true,prefixes[index].start..end)).collect::<Vec<_>>();
            if self.cache.limit<8*1024*1024 && !context.can_pin_ranges(self.block_size,&spans) {return Ok(());}
            if !spans.is_empty() {
                let leases = self.prefetch_ranges(&spans,context,usize::MAX)?;
                if self.cache.limit<8*1024*1024 {context.pin_packet_blocks(leases);}
                for (index,end) in plans {
                    let prefix=&mut prefixes[index];
                    tracing::debug!(tile=prefix.part.tile,cursor=prefix.cursor,end,useful_bytes=prefix.cursor-prefix.start,
                        packets_remaining=prefix.packets.len()-prefix.next,"bounded contiguous LL read-ahead");
                    prefix.fetched_end=end;
                }
            }
            for prefix in &mut prefixes {
                let Some(packet) = prefix.packets.get(prefix.next) else { continue; };
                let Some(blocks) = prefix_codeblocks(&self.header,prefix.part.tile,packet)? else { return Ok(()); };
                let (header,body) = self.probe_packet_length(&prefix.part,prefix.cursor,&blocks,context)?;
                prefix.cursor = prefix.cursor.checked_add(header as u64).and_then(|cursor| cursor.checked_add(body)).context("packet extent overflow")?;
                ensure!(prefix.cursor <= prefix.part.offset+prefix.part.length,"LL packet exceeds tile part");
                prefix.next += 1;
            }
        }
        let bodies = prefixes.iter().map(|prefix| (true,prefix.start..prefix.cursor)).collect::<Vec<_>>();
        if self.cache.limit<8*1024*1024 && !context.can_pin_ranges(self.block_size,&bodies) {return Ok(());}
        let leases = self.prefetch_ranges(&bodies,context,48)?;
        if self.cache.limit<8*1024*1024 {context.pin_packet_blocks(leases);}
        Ok(())
    }

    fn visit_packets(&self,tile:u16,context:&mut ReadContext,mut visit:impl FnMut(&Packet,usize)->Result<()>)->Result<()> {
        if self.header.progression==2 {visit_rpcl_packets(&self.header,tile,visit)?;}
        else {let (packets,_)=self.ordered_packets(tile,None,context)?;for (n,packet) in packets.iter().enumerate() {visit(packet,n)?;}}
        Ok(())
    }
    fn ordered_packets(&self,tile:u16,prefix:Option<u8>,context:&mut ReadContext)->Result<packet_plan_cache::Plan> {
        if let Some(order)=context.orders.get(&(tile,prefix)) {return Ok(order.clone());}
        let (order,_hit)=packet_plan_cache::Cache::shared().get_or_build(&self.header,tile,prefix)?;
        if context.orders.len()<4096 && context.admit_metadata(order.0.capacity()*std::mem::size_of::<Packet>()+96) {context.orders.insert((tile,prefix),order.clone());}
        Ok(order)
    }
    fn probe_packet_length(&self, part: &TilePart, cursor: u64, blocks: &[(usize,usize)], context: &mut ReadContext) -> Result<(usize,u64)> {
        if let Some(length)=context.packet_lengths.get(&cursor) {return Ok(*length);}
        let mut size = 64;
        let mut probe=Vec::new();
        loop {
            let end = (cursor+size).min(part.offset+part.length);
            probe.extend(self.read_packet_probe(cursor+probe.len() as u64..end,context)?);
            match packet_length_marked(&probe,blocks,self.header.is_ht,self.header.cod[8],self.header.cod[0]) {
                Ok(length) => {
                    if context.packet_lengths.len()<16384 && context.admit_metadata(64) {context.packet_lengths.insert(cursor,length);}
                    return Ok(length);
                },
                Err(error) if error.downcast_ref::<ProbeTooShort>().is_some() => {
                    ensure!(end < part.offset+part.length && size < MAX_HEADER as u64,"truncated or oversized inline packet header");
                    size = (size*2).min(MAX_HEADER as u64);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn prefix_payload(&self, part: &TilePart, start: u64, packets: &[Packet], total: usize, reduction: u8,
        roi:Option<&precinct_roi::Selection>,context: &mut ReadContext) -> Result<Option<Vec<u8>>> {
        // Single-layer LRCP/RLCP/RPCL, and the first layer of LRCP,
        // place the requested resolutions in a contiguous prefix.
        // Later layers still require the bounded fallback when unindexed.
        let wanted = self.header.levels - 1 - reduction;
        let count = packets.iter().take_while(|p| p.layer == 0 && p.resolution <= wanted).count();
        let mut bytes = vec![];
        let mut cursor = start;
        let mut ahead = vec![];
        let mut ahead_start = cursor;
        ensure!(roi.is_none() || self.header.layers==1,"inline spatial filtering requires one quality layer");
        let (mut kept,mut omitted_stream_bytes)=(0usize,0u64);
        for (ordinal,packet) in packets.iter().take(count).enumerate() {
            context.check()?;
            let keep=roi.is_none_or(|selection|selection.keeps(&self.header,packet));
            if let Some(&(header_length,body_length))=context.packet_lengths.get(&cursor) {
                let end=cursor.checked_add(header_length as u64).and_then(|n|n.checked_add(body_length)).context("packet extent overflow")?;
                ensure!(end<=part.offset+part.length,"LL packet exceeds tile part");
                if keep {bytes.extend(self.read(cursor..end,context)?);kept+=1;}
                else {append_empty_packet(&mut bytes,self.header.cod[0],ordinal);omitted_stream_bytes+=end-cursor;}
                cursor=end;continue;
            }
            let Some(blocks) = prefix_codeblocks(&self.header, part.tile, packet)? else { return Ok(None); };
            if cursor + 64 > ahead_start + ahead.len() as u64 {
                let budget=context.limit.saturating_sub(context.stats.bytes).saturating_sub(self.block_size.saturating_mul(2));
                let read_ahead = prefix_read_ahead(wanted).max(lowest_prefix_window(cursor-start,ordinal,count,budget));
                let probe_end = (cursor + read_ahead).min(part.offset + part.length as u64);
                ahead = self.read(cursor..probe_end, context)?; ahead_start = cursor;
            }
            let probe = &ahead[(cursor - ahead_start) as usize..];
            let (header_length, body_length) = match packet_length_marked(probe, &blocks,self.header.is_ht,self.header.cod[8],self.header.cod[0]) {
                Ok(length) => length,
                Err(error) if error.downcast_ref::<ProbeTooShort>().is_some() => {
                    // A compact initial probe must not impose a new packet-
                    // header restriction on large native tiles/codeblock grids.
                    let mut size = probe.len() as u64;
                    loop {
                        size = size.saturating_mul(2).max(128).min(MAX_HEADER as u64);
                        let end = (cursor + size).min(part.offset + part.length);
                        ensure!(end > cursor + ahead.len().saturating_sub((cursor - ahead_start) as usize) as u64,
                            "truncated or oversized inline packet header");
                        let consumed=(cursor-ahead_start) as usize;
                        if consumed>0 {ahead.drain(..consumed);}
                        ahead_start=cursor;
                        ahead.extend(self.read(cursor+ahead.len() as u64..end,context)?);
                        match packet_length_marked(&ahead, &blocks,self.header.is_ht,self.header.cod[8],self.header.cod[0]) {
                            Ok(length) => break length,
                            Err(error) if error.downcast_ref::<ProbeTooShort>().is_some() => continue,
                            Err(error) => return Err(error),
                        }
                    }
                }
                Err(error) => return Err(error),
            };
            let end = cursor.checked_add(header_length as u64).and_then(|n|n.checked_add(body_length)).context("packet extent overflow")?;
            ensure!(end <= part.offset + part.length as u64, "LL packet exceeds tile part");
            if context.packet_lengths.len()<16384 && context.admit_metadata(64) {context.packet_lengths.insert(cursor,(header_length,body_length));}
            if !keep {
                // Each precinct's first-layer header has independent state.
                // Parse its true length, then replace only its temporary-view
                // packet; never fetch an unselected body's unbuffered tail.
                append_empty_packet(&mut bytes,self.header.cod[0],ordinal);
                omitted_stream_bytes+=end-cursor;cursor=end;continue;
            }
            kept+=1;
            let buffered_end=end.min(ahead_start+ahead.len() as u64);
            bytes.extend_from_slice(&ahead[(cursor-ahead_start) as usize..(buffered_end-ahead_start) as usize]);
            if buffered_end<end {bytes.extend(self.read(buffered_end..end,context)?);}
            cursor = end;
        }
        let fill=1+usize::from(self.header.cod[0]&2!=0)*6+usize::from(self.header.cod[0]&4!=0)*2;
        let extra=(total-count).checked_mul(fill).context("empty packet size overflow")?;
        ensure!(bytes.len().checked_add(extra).is_some_and(|size|size<=context.limit as usize+MAX_HEADER+MAX_PACKETS),"compressed subset exceeds request memory budget");
        bytes.reserve_exact(extra);
        for ordinal in count..total {append_empty_packet(&mut bytes,self.header.cod[0],ordinal);}
        if std::env::var_os("RIVER_PROFILE_SOURCE").is_some() {
            tracing::info!(source=%self.id,packets=count,kept_packets=kept,omitted_stream_bytes,subset_bytes=bytes.len(),
                "inline resolution-prefix packet selection; omitted stream bytes are not wire savings");
        }
        Ok(Some(bytes))
    }
}

fn payload_range_concurrency(single_native:bool,block_size:u64,bytes:u64,ranges:&[Range<u64>],latency:u64)->usize {
    // Reduce RTT waves without changing ranges or bytes. All lanes still share
    // the source's admission limit; metadata keeps its existing policy.
    if single_native && (5000..u64::MAX).contains(&latency) && (1024*1024..=8*1024*1024).contains(&bytes) && ranges.len()>=8 {16}
    else {range_concurrency(block_size,bytes,ranges,latency)}
}
fn range_concurrency(block_size: u64, bytes: u64, ranges: &[Range<u64>], network_min_us: u64) -> usize {
    let remote_latency = (2000..u64::MAX).contains(&network_min_us);
    // A few merged gaps must not serialize an otherwise tiny-read batch.
    // Total buffered bytes stay capped even when one range is larger.
    let small_average=bytes<=ranges.len() as u64*16*1024;
    if bytes <= 1024*1024 && (block_size == 256 || (remote_latency && small_average)) { 16 } else { 4 }
}

fn identical_coc(payload: &[u8], components: u8, cod: &[u8]) -> bool {
    payload.len() >= 2 && cod.len() >= 5 && payload[0] < components && payload[1] == cod[0] & 1
        && payload[2..] == cod[5..]
}

fn resolved_part_length(declared: u32, offset: u64, end: u64) -> Result<u64> {
    let length = if declared == 0 {
        end.checked_sub(2).and_then(|end| end.checked_sub(offset)).context("invalid final tile-part extent")?
    } else { u64::from(declared) };
    ensure!(length >= 14 && offset.checked_add(length).is_some_and(|last| last <= end.saturating_sub(2)), "unindexed or invalid tile-part length");
    Ok(length)
}

fn adaptive_ranges(ranges:&mut Vec<Range<u64>>,profile:&crate::read_policy::Profile,quota:usize,remaining:u64,useful:u64,
    maximum_range_bytes:u64,execution_concurrency:impl Fn(&[Range<u64>])->usize)->Result<()> {
    use crate::read_policy::{self,Mode,Decision};
    let settings=read_policy::settings();if settings.mode==Mode::Disabled {return Ok(());}
    let observation=profile.snapshot();let bytes:u64=ranges.iter().map(|r|r.end-r.start).sum();
    let mut decision=Decision {reason:"insufficientSamples".into(),sample_count_at_decision:observation.samples,
        throughput_samples_at_decision:observation.throughput_samples,original_requests:ranges.len(),planned_requests:ranges.len(),original_bytes:bytes,planned_bytes:bytes,estimated_original_ms:0.0,estimated_planned_ms:0.0,applied:false};
    decision.reason=if observation.samples<u64::from(settings.minimum_samples) {"insufficientSamples"}
        else if observation.throughput_samples<3 {"insufficientThroughputSamples"}
        else if ranges.len()>16384 {"rangePlannerLimit"}else{"missingLatencyOrThroughput"}.into();
    if observation.samples<u64::from(settings.minimum_samples) || observation.throughput_samples<3 || ranges.len()>16384 {
        profile.decision(decision);return Ok(());
    }
    let (Some(latency),Some(rate))=(observation.latency_ms,observation.planning_bytes_per_second()) else {profile.decision(decision);return Ok(());};
    let concurrency=execution_concurrency(ranges).min(settings.effective_source_requests());
    let baseline=read_policy::estimated_ms(ranges.len(),bytes,concurrency,latency,rate);
    let ratio=u64::from(settings.max_overfetch_ratio).min(crate::read_policy::explicit_overfetch_cap().unwrap_or(16));
    let budget=remaining.min(useful.saturating_mul(ratio));let mut best=ranges.clone();let mut cost=baseline;let mut alternative=false;let mut timeout_risk=false;
    let transfer_cap=if maximum_range_bytes>1024*1024 {
        maximum_range_bytes.min((rate*((8000.0-latency).max(0.0)/1000.0)) as u64)
    }else{maximum_range_bytes};
    // Include small batches: a twelve-range packet demand can benefit from
    // one/few bounded reads too. Keep byte,overfetch and timeout guards intact.
    let targets:&[usize]=&[1,2,4,8,16,32];
    for &target in targets {
        if target>=ranges.len(){continue;}
        let mut candidate=ranges.clone();merge_ranges_bounded(&mut candidate,target,quota,budget,ratio,transfer_cap)?;
        if maximum_range_bytes>1024*1024 && candidate.iter().any(|r|
            read_policy::estimated_ms(1,r.end-r.start,1,latency,rate)>8000.0) {
            timeout_risk=true;continue;
        }
        alternative|=candidate.len()<ranges.len();
        let size=candidate.iter().map(|r|r.end-r.start).sum();
        let proposed=read_policy::estimated_ms(candidate.len(),size,execution_concurrency(&candidate).min(settings.effective_source_requests()),latency,rate);
        if proposed<cost*0.9 {best=candidate;cost=proposed;}
    }
    decision.estimated_original_ms=baseline;decision.estimated_planned_ms=cost;decision.planned_requests=best.len();decision.planned_bytes=best.iter().map(|r|r.end-r.start).sum();
    decision.reason=if best.len()<ranges.len(){"predictedRoundTripSavings"}else if alternative {"extraBytesCostMore"}else if timeout_risk {"rangeTimeoutRisk"}else{"overfetchLimitOrRangeGeometry"}.into();
    if best.len()<ranges.len() && settings.mode==Mode::Adaptive {decision.applied=true;*ranges=best;}
    profile.decision(decision);Ok(())
}

fn merge_ranges(ranges: &mut Vec<Range<u64>>, target: usize, quota: usize, remaining_bytes: u64, amplification: u64) -> Result<()> {
    merge_ranges_bounded(ranges,target,quota,remaining_bytes,amplification,1024*1024)
}
fn merge_ranges_bounded(ranges:&mut Vec<Range<u64>>,target:usize,quota:usize,remaining_bytes:u64,amplification:u64,maximum_range_bytes:u64)->Result<()> {
    if ranges.len() <= target { ensure!(ranges.len() <= quota, "S3 range request limit exceeded"); return Ok(()); }
    let useful: u64 = ranges.iter().map(|range| range.end - range.start).sum();
    let budget = useful.saturating_mul(amplification).min(remaining_bytes);
    let mut total = useful;
    // Linked neighbors plus a min-heap avoid rescanning and shifting the
    // entire range vector after each merge on dense packet selections.
    let mut next: Vec<Option<usize>> = (0..ranges.len()).map(|i| (i + 1 < ranges.len()).then_some(i + 1)).collect();
    let mut previous: Vec<Option<usize>> = (0..ranges.len()).map(|i| i.checked_sub(1)).collect();
    let mut live = vec![true; ranges.len()];
    let mut gaps = std::collections::BinaryHeap::new();
    for i in 0..ranges.len().saturating_sub(1) {
        gaps.push(std::cmp::Reverse((ranges[i + 1].start - ranges[i].end, i, i + 1)));
    }
    let mut count = ranges.len();
    while count > target {
        let Some(std::cmp::Reverse((extra, left, right))) = gaps.pop() else { break; };
        if !live[left] || !live[right] || next[left] != Some(right)
            || ranges[right].start - ranges[left].end != extra { continue; }
        if ranges[right].end - ranges[left].start > maximum_range_bytes { continue; }
        if total.saturating_add(extra) > budget { break; }
        total += extra;
        ranges[left].end = ranges[right].end;
        live[right] = false;
        next[left] = next[right];
        if let Some(after) = next[left] {
            previous[after] = Some(left);
            gaps.push(std::cmp::Reverse((ranges[after].start - ranges[left].end, left, after)));
        }
        if let Some(before) = previous[left] {
            gaps.push(std::cmp::Reverse((ranges[left].start - ranges[before].end, before, left)));
        }
        count -= 1;
    }
    let mut index = 0;
    ranges.retain(|_| { let keep = live[index]; index += 1; keep });
    ensure!(ranges.len() <= quota, "S3 range request limit exceeded without excessive overfetch; source needs a packet index or smaller viewport");
    Ok(())
}

fn trim_retained_index(index: &mut PartIndex, first: u64, budget: usize) {
    if index.parts.capacity().saturating_mul(std::mem::size_of::<TilePart>()) > budget {
        *index = PartIndex { cursor: first, ..PartIndex::default() };
    }
}

fn prefix_read_ahead(wanted: u8) -> u64 {
    match wanted { 0 => 64, 1 | 2 => 8 * 1024, _ => 64 * 1024 }
}
fn contiguous_block_ranges(blocks:impl IntoIterator<Item=u64>,block_size:u64,source_end:u64,limit:u64)->Vec<Range<u64>> {
    let mut ranges:Vec<Range<u64>>=vec![];
    for block in blocks {
        if let Some(last)=ranges.last_mut().filter(|r|r.end==block && block+block_size-r.start<=limit) {
            last.end=(block+block_size).min(source_end);
        }else {ranges.push(block..(block+block_size).min(source_end));}
    }
    ranges
}
fn parallel_contiguous_limit(learned:u64,bytes:u64,lanes:usize,block:u64)->u64 {
    learned.min(bytes.div_ceil(lanes.max(1) as u64).div_ceil(block)*block).max(1024*1024)
}
fn tlm_header_read_end(code:u16,start:u64,required_end:u64,source_end:u64,adaptive:bool,remaining:u64,block:u64)->u64 {
    let length=required_end.saturating_sub(start);
    if !adaptive || code!=0xff55 || length<32770 || required_end>source_end {return required_end;}
    let end=required_end.saturating_add(length/2).min(source_end);
    let rounded=end.div_ceil(block).saturating_mul(block).min(source_end).saturating_sub(start/block*block);
    if rounded<=remaining {end}else{required_end}
}
fn prefix_bootstrap_count(samples:u64,throughput_samples:u64,minimum:u32,ranges:usize,quota:usize)->Option<usize> {
    if throughput_samples<3 || samples>=u64::from(minimum) {return None;}
    let needed=usize::try_from(u64::from(minimum)-samples).ok()?;
    (needed<ranges && needed<=quota).then_some(needed)
}
fn payload_bootstrap_indices(ranges:&[Range<u64>],samples:u64,throughput:u64,minimum:u32,quota:usize,latency_us:u64,refresh_mature:bool)->Option<Vec<usize>> {
    if latency_us<2000 || latency_us==u64::MAX {return None;}
    // Tight budgets need the existing merge before any GET is spent. Staging
    // must not consume slots that can make an otherwise valid plan fail.
    if ranges.len()>quota {return None;}
    let mut need_body=usize::try_from(3u64.saturating_sub(throughput)).ok()?;
    let mut count=usize::try_from(u64::from(minimum).saturating_sub(samples)).ok()?.max(need_body);
    if count==0 && refresh_mature {need_body=1;count=1;}
    if count==0 || count>=ranges.len() || count>quota {return None;}
    let mut order=(0..ranges.len()).collect::<Vec<_>>();order.sort_by_key(|&i|ranges[i].end.saturating_sub(ranges[i].start));
    let mut chosen=order.iter().copied().filter(|&i|ranges[i].end-ranges[i].start>=crate::read_policy::THROUGHPUT_SAMPLE_BYTES).take(need_body).collect::<Vec<_>>();
    if chosen.len()!=need_body {return None;}
    for i in order {if chosen.len()==count {break;}if !chosen.contains(&i) {chosen.push(i);}}
    Some(chosen)
}

// Large inline LL prefixes are contiguous. Grow only after consuming useful
// bytes, cap speculative tails, and reduce the last window using observed
// packet size. Tiny native tiles keep the established 64-byte probe.
fn lowest_prefix_window(consumed:u64,parsed:usize,total:usize,budget:u64)->u64 {
    if total<=32 || parsed==0 {return 64;}
    let remaining=total.saturating_sub(parsed) as u64;
    let predicted=consumed.div_ceil(parsed as u64).saturating_mul(remaining).saturating_mul(2).max(64);
    consumed.saturating_mul(4).clamp(64*1024,4*1024*1024).min(predicted).min(budget.max(64))
}

fn resize_siz(main: &mut [u8], width: u32, height: u32) -> Result<()> {
    let mut cursor = 2;
    while cursor + 4 <= main.len() {
        let marker = be16(main, cursor)?; let length = be16(main, cursor + 2)? as usize;
        ensure!(length >= 2 && cursor + 2 + length <= main.len(), "invalid subset main header");
        if marker == 0xff51 {
            main[cursor + 6..cursor + 10].copy_from_slice(&width.to_be_bytes());
            main[cursor + 10..cursor + 14].copy_from_slice(&height.to_be_bytes());
            return Ok(());
        }
        cursor += 2 + length;
    }
    bail!("subset SIZ marker missing")
}
pub(crate) fn resize_jp2(boxes: &mut [u8], width: u32, height: u32) -> Result<()> {
    let mut cursor = 0;
    while cursor + 8 <= boxes.len() {
        let small = be32(boxes, cursor)? as usize;
        let header = if small == 1 { 16 } else { 8 };
        let size = if small == 1 { usize::try_from(be64(boxes, cursor + 8)?)? } else { small };
        ensure!(size >= header && cursor + size <= boxes.len(), "invalid essential JP2 box");
        if &boxes[cursor + 4..cursor + 8] == b"jp2h" { resize_jp2(&mut boxes[cursor + header..cursor + size], width, height)?; }
        else if &boxes[cursor + 4..cursor + 8] == b"ihdr" {
            ensure!(size >= header + 8, "truncated JP2 image header");
            boxes[cursor + header..cursor + header + 4].copy_from_slice(&height.to_be_bytes());
            boxes[cursor + header + 4..cursor + header + 8].copy_from_slice(&width.to_be_bytes());
        }
        cursor += size;
    }
    Ok(())
}

#[derive(Clone)]
struct RangeClient {budget:Arc<wire_budget::Budget>,trace_id:u64,object_id:String,caller:&'static std::panic::Location<'static>,block_size:u64,part:Option<usize>,store:Arc<dyn ObjectStore>,location:Path,etag:Option<String>,version:Option<String>,network_min_us:Arc<AtomicU64>,profile:Arc<crate::read_policy::Profile>}
impl RangeClient {
    async fn fetch(&self,range:Range<u64>,deadline:Instant,cancel:Option<watch::Receiver<bool>>)->Result<Vec<u8>> {
        self.fetch_with_network_timeout(range,deadline,cancel,Duration::from_secs(10)).await
    }
    async fn fetch_with_network_timeout(&self, range: Range<u64>, deadline: Instant, mut cancel: Option<watch::Receiver<bool>>,network_timeout:Duration) -> Result<Vec<u8>> {
        let mut range_trace=read_trace::Read::begin(self.trace_id,&self.object_id,self.caller,"batchedRange",range.clone(),self.block_size,None,self.part,"sdk");
        let opts = GetOptions { range: Some(GetRange::Bounded(range.clone())),
            if_match: self.etag.clone(), version: self.version.clone(), ..Default::default() };
        let get = async {
            let _source_permit=self.profile.acquire().await;
            range_trace.admitted();
            ensure!(Instant::now()<deadline,"S3 range deadline exceeded during source admission");
            let network_started = Instant::now();
            let result = tokio::time::timeout(network_timeout.min(deadline.saturating_duration_since(Instant::now())),
                self.budget.scope(range_trace.scope(self.store.get_opts(&self.location, opts)))).await
                .context("S3 range timed out waiting for response headers")??;
            ensure!(result.range == range, "S3 endpoint ignored or changed the requested byte range");
            let header_time=network_started.elapsed();
            // Large useful ranges may take longer under source load. Require
            // continuing byte progress; the outer operation deadline and
            // cancellation still bound the entire transfer and admission.
            let body = body_with_progress_timeout(result.into_stream(),range.end-range.start,network_timeout).await?;
            let elapsed=network_started.elapsed();
            range_trace.transfer_timing(header_time,elapsed);
            self.profile.record(body.len() as u64,header_time,elapsed);
            self.network_min_us.fetch_min(elapsed.as_micros().min(u128::from(u64::MAX)) as u64, Ordering::Relaxed);
            Ok::<_, anyhow::Error>(body)
        };
        let cancelled = async {
            if let Some(receiver) = &mut cancel {
                if *receiver.borrow() { return; }
                let _ = receiver.changed().await;
            } else { std::future::pending::<()>().await; }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result=tokio::select! {
            result = tokio::time::timeout(remaining, get) => result.context("S3 range timed out waiting for source admission or transfer deadline").and_then(|r|r),
            _ = cancelled => Err(anyhow::anyhow!("S3 request superseded")),
        };
        range_trace.finish(result.as_ref().map_or(0,|b|b.len() as u64),result.is_ok(),None);
        result
    }
}

async fn bounded_body(result: object_store::GetResult, length: u64) -> Result<Vec<u8>> {
    let mut stream = result.into_stream();
    let mut bytes = Vec::with_capacity(usize::try_from(length)?);
    while let Some(chunk) = stream.try_next().await? {
        ensure!(bytes.len() as u64 + chunk.len() as u64 <= length, "S3 response exceeds requested range");
        bytes.extend_from_slice(&chunk);
    }
    ensure!(bytes.len() as u64 == length, "incomplete S3 range response");
    Ok(bytes)
}
async fn body_with_progress_timeout<S,T,E>(mut stream:S,length:u64,idle:Duration)->Result<Vec<u8>>
where S:futures_util::Stream<Item=std::result::Result<T,E>>+Unpin,T:AsRef<[u8]>,E:Into<anyhow::Error> {
    let mut bytes=Vec::with_capacity(usize::try_from(length)?);
    let mut progress_deadline=Instant::now()+idle;
    loop {
        ensure!(Instant::now()<progress_deadline,"S3 range timed out waiting for body progress");
        let next=tokio::time::timeout(progress_deadline.saturating_duration_since(Instant::now()),stream.next()).await
            .context("S3 range timed out waiting for body progress")?;
        let Some(chunk)=next else {break;};let chunk=chunk.map_err(Into::into)?;let chunk=chunk.as_ref();
        ensure!(bytes.len() as u64+chunk.len() as u64<=length,"S3 response exceeds requested range");
        if !chunk.is_empty() {bytes.extend_from_slice(chunk);progress_deadline=Instant::now()+idle;}
    }
    ensure!(bytes.len() as u64==length,"incomplete S3 range response");Ok(bytes)
}

// Queue misses fairly: a serial SOT walker must not repeatedly reacquire an
// unfair try-lock ahead of a payload request whose index is already ready.
async fn fetch_gate(lock:&tokio::sync::Mutex<()>,deadline:Instant,mut cancel:Option<watch::Receiver<bool>>)->Result<tokio::sync::MutexGuard<'_,()>> {
    let cancelled=async {
        if let Some(receiver)=&mut cancel {
            if *receiver.borrow() {return;}
            let _=receiver.changed().await;
        } else {std::future::pending::<()>().await;}
    };
    tokio::select! {
        guard=tokio::time::timeout(deadline.saturating_duration_since(Instant::now()),lock.lock())=>Ok(guard.context("S3 range admission timed out")?),
        _=cancelled=>bail!("S3 request superseded"),
    }
}

fn lock_cancellable<'a, T>(lock: &'a Mutex<T>, context: &ReadContext) -> Result<std::sync::MutexGuard<'a, T>> {
    loop {
        context.check()?;
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

fn be16(data: &[u8], offset: usize) -> Result<u16> { Ok(u16::from_be_bytes(data.get(offset..offset + 2).context("truncated JPEG 2000 field")?.try_into()?)) }
fn be32(data: &[u8], offset: usize) -> Result<u32> { Ok(u32::from_be_bytes(data.get(offset..offset + 4).context("truncated JPEG 2000 field")?.try_into()?)) }
fn be64(data: &[u8], offset: usize) -> Result<u64> { Ok(u64::from_be_bytes(data.get(offset..offset + 8).context("truncated JPEG 2000 field")?.try_into()?)) }
fn parse_tlm(payload: &[u8]) -> Result<Vec<(Option<u16>, u32)>> {
    let style = *payload.first().context("empty TLM")?;
    let tile_size = (style >> 4) & 3; ensure!(tile_size <= 2, "invalid TLM tile width");
    let length_size = if style & 0x40 != 0 { 4 } else { 2 };
    let mut cursor = 1; let mut entries = vec![];
    while cursor < payload.len() {
        let tile = match tile_size { 0 => None, 1 => Some(*payload.get(cursor).context("truncated TLM")? as u16), _ => Some(be16(payload, cursor)?) };
        cursor += tile_size as usize;
        let length = if length_size == 4 { be32(payload, cursor)? } else { be16(payload, cursor)? as u32 };
        cursor += length_size; entries.push((tile, length));
    }
    Ok(entries)
}
fn tlm_offsets(start: u64, end: u64, entries: &[(Option<u16>,u32)]) -> Result<Vec<u64>> {
    ensure!(entries.len() <= 100_000,"too many TLM tile parts");
    let mut offsets = Vec::with_capacity(entries.len());
    let mut cursor = start;
    for &(_,length) in entries {
        ensure!(length >= 14,"invalid TLM tile-part length");
        offsets.push(cursor);
        cursor = cursor.checked_add(u64::from(length)).context("TLM extent overflow")?;
        ensure!(cursor <= end.saturating_sub(2),"TLM tile-part exceeds codestream");
    }
    Ok(offsets)
}
fn parse_plt(bytes: &[u8]) -> Result<Vec<u64>> {
    let mut lengths = vec![]; let mut value = 0u64; let mut pending = false;
    for &byte in bytes {
        ensure!(value <= u32::MAX as u64 >> 7, "PLT packet length overflow");
        value = (value << 7) | u64::from(byte & 127); pending = byte & 128 != 0;
        if !pending { lengths.push(value); value = 0; ensure!(lengths.len() <= MAX_PACKETS, "too many packet lengths"); }
    }
    ensure!(!pending, "truncated PLT packet length"); Ok(lengths)
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Packet { resolution: u8, layer: u16, component: u8, x: u64, y: u64 }
fn header_window_prefix(range:Range<u64>,ahead:&Option<(u64,Vec<u8>)>)->Vec<u8> {
    if let Some((start,bytes))=ahead {
        if range.start>=*start && range.start<start.saturating_add(bytes.len() as u64) {
            let from=(range.start-start) as usize;let count=(range.end-range.start).min((bytes.len()-from) as u64) as usize;
            return bytes[from..from+count].to_vec();
        }
    }
    vec![]
}
// Allocation-free geometry count; used only as a bounded read-ahead hint.
fn tile_packet_count(h:&Header,tile:u16)->Option<u64> {
    let (x0,y0,x1,y1)=h.tile_bounds(tile);let mut total=0u64;
    for resolution in 0..h.levels {
        let scale=1u32.checked_shl(u32::from(h.levels-1-resolution))?;
        let pp=if h.cod[0]&1!=0 {*h.cod.get(10+resolution as usize)?}else{0xff};
        let pw=1u32<<(pp&15);let ph=1u32<<(pp>>4);
        let count=u64::from(x1.div_ceil(scale).div_ceil(pw).checked_sub(x0.div_ceil(scale)/pw)?)
            .checked_mul(u64::from(y1.div_ceil(scale).div_ceil(ph).checked_sub(y0.div_ceil(scale)/ph)?))?
            .checked_mul(u64::from(h.components))?.checked_mul(u64::from(h.layers))?;
        total=total.checked_add(count)?;
    }
    Some(total)
}
fn plt_read_ahead_end(cursor:u64,required:u64,part_end:u64,header_end:u64,packets:u64,adaptive:bool,latency_us:u64,ratio:u64,remaining:u64)->u64 {
    if !adaptive || latency_us<5000 || required-cursor<65530 || packets<65536 || ratio<=1 {return required;}
    // Each advertised packet requires at least one PLT byte. Geometry is a hint,
    // never an index: every subsequent marker and packet length is still parsed.
    let bytes=packets.saturating_mul(ratio).min(MAX_HEADER as u64).min(remaining);
    required.max(cursor.saturating_add(bytes).min(part_end).min(header_end))
}
// RPCL already has the natural construction order. Stream descriptors for
// one-pass plans instead of allocating and then reading the full packet vector.
fn visit_rpcl_packets(h:&Header,tile:u16,mut visit:impl FnMut(&Packet,usize)->Result<()>)->Result<usize> {
    ensure!(h.progression==2,"streamed packet traversal requires RPCL");
    let (x0,y0,x1,y1)=h.tile_bounds(tile);ensure!(x0<x1 && y0<y1,"invalid tile index");
    let total=tile_packet_count(h,tile).context("packet count overflow")?;ensure!(total<=MAX_PACKETS as u64,"packet index exceeds request memory budget");
    let mut ordinal=0;
    for resolution in 0..h.levels {
        let scale=1u32<<(h.levels-1-resolution);let pp=if h.cod[0]&1!=0 {h.cod[10+resolution as usize]}else{0xff};
        let pw=1u32<<(pp&15);let ph=1u32<<(pp>>4);
        for py in y0.div_ceil(scale)/ph..y1.div_ceil(scale).div_ceil(ph) {
            for px in x0.div_ceil(scale)/pw..x1.div_ceil(scale).div_ceil(pw) {
                for component in 0..h.components {for layer in 0..h.layers {
                    let packet=Packet {resolution,layer,component,x:(u64::from(px)*u64::from(pw)*u64::from(scale)).max(u64::from(x0)),y:(u64::from(py)*u64::from(ph)*u64::from(scale)).max(u64::from(y0))};
                    visit(&packet,ordinal)?;ordinal+=1;
                }}
            }
        }
    }
    ensure!(ordinal as u64==total,"packet traversal count mismatch");Ok(ordinal)
}
fn packet_order(header: &Header, tile: u16, prefix: Option<u8>) -> Result<(Vec<Packet>, usize)> {
    packet_order_layers(header,tile,prefix,false)
}
fn packet_order_layers(header: &Header, tile: u16, prefix: Option<u8>, all_layers:bool) -> Result<(Vec<Packet>, usize)> {
    let h = header;
    let (x0,y0,x1,y1) = h.tile_bounds(tile);
    ensure!(x0 < x1 && y0 < y1, "invalid tile index");
    let mut packets = vec![];
    let mut total = 0u64;
    for resolution in 0..h.levels {
        let scale = 1u32 << (h.levels - 1 - resolution);
        let pp = if h.cod[0] & 1 != 0 { h.cod[10 + resolution as usize] } else { 0xff };
        let pw = 1u32 << (pp & 15); let ph = 1u32 << (pp >> 4);
        let rx0 = x0.div_ceil(scale); let ry0 = y0.div_ceil(scale);
        let rx1 = x1.div_ceil(scale); let ry1 = y1.div_ceil(scale);
        let count = u64::from(rx1.div_ceil(pw) - rx0 / pw)
            .checked_mul(u64::from(ry1.div_ceil(ph) - ry0 / ph))
            .and_then(|count| count.checked_mul(u64::from(h.components)))
            .and_then(|count| count.checked_mul(u64::from(h.layers)))
            .context("packet count overflow")?;
        total = total.checked_add(count).context("packet count overflow")?;
        if prefix.is_some_and(|wanted| resolution > wanted) { continue; }
        let materialized_layers = if prefix.is_some() && !all_layers { 1 } else { h.layers };
        // Counting omitted resolutions needs no packet storage. Apply the
        // allocation bound to the actual prefix/layers being materialized;
        // a fine-precinct source can have millions of unrelated packets.
        let materialized=count/u64::from(h.layers)*u64::from(materialized_layers);
        ensure!((packets.len() as u64).saturating_add(materialized)<=MAX_PACKETS as u64,
            "packet index exceeds request memory budget");
        packets.reserve(usize::try_from(materialized).context("packet index exceeds address space")?);
        for py in ry0 / ph..ry1.div_ceil(ph) {
            for px in rx0 / pw..rx1.div_ceil(pw) {
                for component in 0..h.components { for layer in 0..materialized_layers {
                    packets.push(Packet { resolution, layer, component,
                        x: (u64::from(px) * u64::from(pw) * u64::from(scale)).max(u64::from(x0)),
                        y: (u64::from(py) * u64::from(ph) * u64::from(scale)).max(u64::from(y0)) });
                } }
            }
        }
    }
    // Every packet has unique layer/resolution/component/precinct coordinates;
    // stability adds scratch storage without affecting progression order.
    // The construction loops are already resolution/position/component/layer
    // order. RPCL can use them directly, including multi-layer edge tiles.
    if h.progression != 2 {
    packets.sort_unstable_by_key(|p| match h.progression {
        0 => [p.layer as u64, p.resolution as u64, p.component as u64, p.y, p.x],
        1 => [p.resolution as u64, p.layer as u64, p.component as u64, p.y, p.x],
        2 => [p.resolution as u64, p.y, p.x, p.component as u64, p.layer as u64],
        3 => [p.y, p.x, p.component as u64, p.resolution as u64, p.layer as u64],
        _ => [p.component as u64, p.y, p.x, p.resolution as u64, p.layer as u64],
    });
    }
    Ok((packets, usize::try_from(total).context("packet count exceeds address space")?))
}
fn prefix_codeblocks(h: &Header, tile: u16, packet: &Packet) -> Result<Option<Vec<(usize, usize)>>> {
    let r = packet.resolution;
    let (tx0,ty0,tx1,ty1) = h.tile_bounds(tile);
    let pp = if h.cod[0] & 1 != 0 { h.cod[10 + r as usize] } else { 0xff };
    if r > 0 && (pp & 15 == 0 || pp >> 4 == 0) { return Ok(None); }
    let resolution_scale = 1u64 << (h.levels - 1 - r);
    let px = packet.x / resolution_scale / (1u64 << (pp & 15));
    let py = packet.y / resolution_scale / (1u64 << (pp >> 4));
    let bands: &[u8] = if r == 0 { &[0] } else { &[1, 2, 3] };
    let mut grids = vec![];
    for &band in bands {
        let level = h.levels - 1 - r + u8::from(r > 0);
        let denominator = 1i64 << level;
        let shift = if r > 0 { 1i64 << (level - 1) } else { 0 };
        let x_shift = i64::from(band & 1) * shift;
        let y_shift = i64::from(band >> 1) * shift;
        let ceil = |n: i64| -(-n).div_euclid(denominator);
        let bx0 = ceil(i64::from(tx0) - x_shift); let bx1 = ceil(i64::from(tx1) - x_shift);
        let by0 = ceil(i64::from(ty0) - y_shift); let by1 = ceil(i64::from(ty1) - y_shift);
        let pw = 1i64 << ((pp & 15) - u8::from(r > 0));
        let ph = 1i64 << ((pp >> 4) - u8::from(r > 0));
        let lo_x = bx0.max(px as i64 * pw); let hi_x = bx1.min((px as i64 + 1) * pw);
        let lo_y = by0.max(py as i64 * ph); let hi_y = by1.min((py as i64 + 1) * ph);
        if lo_x >= hi_x || lo_y >= hi_y { continue; }
        let cbw = (1i64 << (h.cod[6] + 2)).min(pw);
        let cbh = (1i64 << (h.cod[7] + 2)).min(ph);
        let width = (-(-hi_x).div_euclid(cbw) - lo_x.div_euclid(cbw)) as usize;
        let height = (-(-hi_y).div_euclid(cbh) - lo_y.div_euclid(cbh)) as usize;
        if width * height > 16384 { return Ok(None); }
        grids.push((width, height));
    }
    Ok(Some(grids))
}

#[derive(Clone)]
struct TagNode { value: u32, low: u32, parent: Option<usize> }
#[derive(Clone)]
struct TagTree { nodes: Vec<TagNode>, single:TagNode }
impl TagTree {
    fn new(mut width: usize, mut height: usize) -> Self {
        let single=TagNode {value:u32::MAX,low:0,parent:None};
        if width==1 && height==1 {return Self {nodes:vec![],single};}
        let mut nodes = vec![]; let mut offset = 0;
        loop {
            let next_width = width.div_ceil(2); let next_height = height.div_ceil(2);
            let count = width * height;
            for y in 0..height { for x in 0..width {
                nodes.push(TagNode { value: u32::MAX, low: 0,
                    parent: (count > 1).then_some(offset + count + y / 2 * next_width + x / 2) });
            } }
            if count <= 1 { break; }
            offset += count; width = next_width; height = next_height;
        }
        Self { nodes, single }
    }
    fn decode(&mut self, leaf: usize, threshold: u32, bits: &mut Bits<'_>) -> Result<bool> {
        if self.nodes.is_empty() {
            ensure!(leaf==0,"invalid single-node tag-tree leaf");
            while self.single.low<threshold && self.single.low<self.single.value {
                if bits.bit()?!=0 {self.single.value=self.single.low;} else {self.single.low+=1;}
            }
            return Ok(self.single.value<threshold);
        }
        // Geometry is bounded to 16,384 codeblocks per subband. A fixed
        // ancestry stack avoids a heap allocation for every codeblock lookup.
        let mut path = [0usize; 32];
        path[0] = leaf;
        let mut depth = 1;
        while let Some(parent) = self.nodes[path[depth - 1]].parent {
            ensure!(depth < path.len(), "packet tag tree exceeds supported depth");
            path[depth] = parent;
            depth += 1;
        }
        let mut low = 0;
        for &index in path[..depth].iter().rev() {
            let node = &mut self.nodes[index]; node.low = node.low.max(low);
            while node.low < threshold && node.low < node.value {
                if bits.bit()? != 0 { node.value = node.low; } else { node.low += 1; }
            }
            low = node.low;
        }
        Ok(self.nodes[leaf].value < threshold)
    }
}

#[derive(Debug)]
struct ProbeTooShort;
impl std::fmt::Display for ProbeTooShort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { formatter.write_str("LL packet header exceeds probe") }
}
impl std::error::Error for ProbeTooShort {}
struct Bits<'a> { bytes: &'a [u8], offset: usize, remaining: u8, byte: u8, previous: u8 }
impl Bits<'_> {
    fn bit(&mut self) -> Result<u8> {
        if self.remaining == 0 { self.byte = *self.bytes.get(self.offset).ok_or(ProbeTooShort)?;
            self.offset += 1; self.remaining = if self.previous == 255 { 7 } else { 8 }; self.previous = self.byte; }
        self.remaining -= 1; Ok((self.byte >> self.remaining) & 1)
    }
    fn bits(&mut self, count: u32) -> Result<u64> { ensure!(count <= 63, "invalid packet bit count"); let mut value = 0; for _ in 0..count { value = value << 1 | self.bit()? as u64; } Ok(value) }
}
// SOP is optional when signalled; EPH is mandatory after each packet header.
// Preserve real packet bytes and emit correctly marked empty replacements.
fn append_empty_packet(bytes:&mut Vec<u8>,flags:u8,ordinal:usize) {
    if flags&2!=0 {bytes.extend_from_slice(&[0xff,0x91,0,4]);bytes.extend_from_slice(&(ordinal as u16).to_be_bytes());}
    bytes.push(0);
    if flags&4!=0 {bytes.extend_from_slice(&[0xff,0x92]);}
}
fn packet_length_marked(bytes:&[u8],grids:&[(usize,usize)],ht:bool,style:u8,flags:u8)->Result<(usize,u64)> {
    let mut prefix=0;
    if flags&2!=0 && bytes.first()==Some(&0xff) {
        let marker=bytes.get(..2).ok_or(ProbeTooShort)?;
        if marker==[0xff,0x91] {
            let sop=bytes.get(..6).ok_or(ProbeTooShort)?;
            ensure!(sop[2..4]==[0,4],"invalid SOP marker length");prefix=6;
        }
    }
    let (header,body)=packet_length_coding(&bytes[prefix..],grids,ht,style)?;
    let mut end=prefix+header;
    if flags&4!=0 {
        let eph=bytes.get(end..end+2).ok_or(ProbeTooShort)?;
        ensure!(eph==[0xff,0x92],"missing required EPH packet marker");end+=2;
    }
    Ok((end,body))
}

fn packet_length_coding(bytes: &[u8], grids: &[(usize, usize)], ht: bool, style: u8) -> Result<(usize, u64)> {
    let mut bits = Bits { bytes, offset: 0, remaining: 0, byte: 0, previous: 0 }; let mut length = 0;
    if bits.bit()? != 0 {
      for &(width, height) in grids {
        let mut inclusion = TagTree::new(width, height);
        let mut zero_planes = TagTree::new(width, height);
        for block in 0..width * height {
        if !inclusion.decode(block, 1, &mut bits)? { continue; }
        ensure!(zero_planes.decode(block, 32, &mut bits)?, "invalid zero-bitplane tag tree");
        let passes = if bits.bit()? == 0 { 1 } else if bits.bit()? == 0 { 2 } else {
            let value = bits.bits(2)?; if value < 3 { 3 + value } else { let value = bits.bits(5)?; if value < 31 { 6 + value } else { 37 + bits.bits(7)? } }
        };
        let mut lblock = 3; while bits.bit()? != 0 { lblock += 1; ensure!(lblock <= 31, "invalid codeblock length"); }
        if ht {
            let placeholders = (passes-1)/3*3;
            let cleanup = bits.bits(lblock + (64-(placeholders+1).leading_zeros())-1)?;
            ensure!((2..65535).contains(&cleanup),"invalid HT cleanup segment length");
            length += cleanup;
            let actual_passes = passes-placeholders;
            if actual_passes > 1 {
                let refinement = bits.bits(lblock + u32::from(actual_passes == 3))?;
                ensure!(refinement < 2047,"invalid HT refinement segment length");
                length += refinement;
            }
        } else {
            // First-layer codeblocks start a fresh segment. TERMALL terminates
            // each pass; BYPASS starts with ten, then alternates two and one.
            let mut remaining = passes;
            let mut maximum = if style & 4 != 0 { 1u64 } else if style & 1 != 0 { 10 } else { 109 };
            while remaining > 0 {
                let segment = remaining.min(maximum);
                let count = lblock + 63 - segment.leading_zeros();
                ensure!(count <= 32, "invalid codeblock segment length bits");
                length = length.checked_add(bits.bits(count)?).context("packet body length overflow")?;
                remaining -= segment;
                if style & 4 == 0 && style & 1 != 0 { maximum = if maximum == 10 || maximum == 1 { 2 } else { 1 }; }
            }
        }
        }
      }
    }
    if bits.previous == 255 { bits.offset += 1; }
    Ok((bits.offset, length))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn main_tlm_lookahead_is_optional_bounded_and_not_used_for_other_markers() {
        assert_eq!(tlm_header_read_end(0xff55,100,65632,200000,true,99328,1024),98398);
        assert_eq!(tlm_header_read_end(0xff55,100,65632,200000,true,99327,1024),65632);
        assert_eq!(tlm_header_read_end(0xff55,100,65632,70000,true,70000,1024),70000);
        assert_eq!(tlm_header_read_end(0xff55,100,65632,200000,false,200000,1024),65632);
        assert_eq!(tlm_header_read_end(0xff55,100,32869,200000,true,200000,1024),32869);
        for code in [0xff51,0xff52,0xff58,0xff64] {
            assert_eq!(tlm_header_read_end(code,100,65632,200000,true,200000,1024),65632);
        }
    }
    #[test] fn larger_contiguous_chunks_preserve_exact_required_bytes_and_holes() {
        let blocks=(0..5120).map(|n|n*1024).chain([7*1024*1024,7*1024*1024+1024,8*1024*1024,8*1024*1024+1024]).collect::<BTreeSet<_>>();
        let end=8*1024*1024+1536;
        let small=contiguous_block_ranges(blocks.clone(),1024,end,1024*1024);
        let large=contiguous_block_ranges(blocks,1024,end,4*1024*1024);
        assert_eq!(large,vec![0..4*1024*1024,4*1024*1024..5*1024*1024,7*1024*1024..7*1024*1024+2048,8*1024*1024..end]);
        assert_eq!(small.iter().map(|r|r.end-r.start).sum::<u64>(),large.iter().map(|r|r.end-r.start).sum::<u64>());
        assert_eq!(small.len(),7);assert_eq!(large.len(),4);
    }
    #[test] fn contiguous_growth_retains_transfer_lanes() {
        assert_eq!(parallel_contiguous_limit(4*1024*1024,7*1024*1024,4,1024),7*1024*1024/4);
        assert_eq!(parallel_contiguous_limit(4*1024*1024,3*1024*1024,4,1024),1024*1024);
        assert_eq!(parallel_contiguous_limit(4*1024*1024,7*1024*1024,1,1024),4*1024*1024);
        assert_eq!(parallel_contiguous_limit(1024*1024,7*1024*1024,4,1024),1024*1024);
    }
    #[test] fn coarse_packet_order_bounds_materialized_prefix_not_unrequested_resolutions() {
        let mut cod=vec![1,2,0,10,1,8,4,4,4,1];cod.extend([0x22;9]);
        let h=Header {image_origin:(0,0),width:1024,height:1024,components:3,bit_depth:8,levels:9,layers:10,
            layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:1,rows:1},
            progression:2,is_ht:false,cod,first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:false,payload_digest:[0;32]};
        let (packets,total)=packet_order_layers(&h,0,Some(0),true).unwrap();
        assert_eq!(total,2_621_430);assert_eq!(packets.len(),30);
        assert!(packets.iter().all(|p|p.resolution==0));
        let (first_layer,expected)=packet_order_layers(&h,0,Some(0),false).unwrap();
        assert_eq!(expected,total);assert_eq!(first_layer.len(),3);
        assert!(first_layer.iter().all(|p|p.layer==0));
        assert!(packet_order_layers(&h,0,None,true).unwrap_err().to_string().contains("packet index exceeds request memory budget"));
    }
    #[test] fn prefix_bootstrap_never_adds_probes_or_ignores_sample_and_request_limits() {
        assert_eq!(prefix_bootstrap_count(5,3,8,32,64),Some(3));
        assert_eq!(prefix_bootstrap_count(5,3,16,32,64),Some(11));
        assert_eq!(prefix_bootstrap_count(5,2,8,32,64),None);
        assert_eq!(prefix_bootstrap_count(8,3,8,32,64),None);
        assert_eq!(prefix_bootstrap_count(5,3,8,3,64),None);
        assert_eq!(prefix_bootstrap_count(5,3,8,32,2),None);
    }
    #[test] fn payload_learning_uses_only_required_ranges_and_respects_cold_source_limits() {
        let ranges=(0..12).map(|i|i*100000..i*100000+if i<4 {1024}else{40000+i*1024}).collect::<Vec<Range<u64>>>();
        let selected=payload_bootstrap_indices(&ranges,2,0,8,64,20000,false).unwrap();
        assert_eq!(selected.len(),6);assert_eq!(&selected[..3],&[4,5,6]);
        assert_eq!(selected.iter().copied().collect::<HashSet<_>>().len(),selected.len());
        assert!(payload_bootstrap_indices(&ranges,2,0,8,64,1000,false).is_none());
        assert!(payload_bootstrap_indices(&ranges,2,0,8,64,u64::MAX,false).is_none());
        assert!(payload_bootstrap_indices(&ranges,8,3,8,64,20000,false).is_none());
        assert_eq!(payload_bootstrap_indices(&ranges,8,3,8,64,20000,true).unwrap(),vec![4]);
        assert!(payload_bootstrap_indices(&ranges,8,3,8,11,20000,true).is_none());
        assert!(payload_bootstrap_indices(&ranges,2,0,8,5,20000,false).is_none());
        assert!(payload_bootstrap_indices(&ranges,2,0,8,11,20000,false).is_none());
        assert!(payload_bootstrap_indices(&ranges[..6],2,0,8,64,20000,false).is_none());
        assert!(payload_bootstrap_indices(&ranges[..4],6,0,8,64,20000,false).is_none());
    }
    #[test] fn pin_budget_counts_only_selected_blocks_and_still_enforces_its_limit() {
        let context=ReadContext::new(None);
        assert!(context.can_pin_ranges(1024,&[(false,0..100*1024*1024),(true,1024..4*1024*1024)]));
        assert!(!context.can_pin_ranges(1024,&[(false,0..1024),(true,0..8*1024*1024+1)]));
        assert!(context.can_pin_ranges(1024,&[(true,0..8*1024*1024),(true,0..8*1024*1024)]));
    }
    #[test]
    fn inline_ll_windows_preserve_tiny_tiles_and_bound_growth_and_tail() {
        assert_eq!(lowest_prefix_window(0,0,1920,u64::MAX),64);
        assert_eq!(lowest_prefix_window(4096,1,3,u64::MAX),64);
        assert_eq!(lowest_prefix_window(1,1,1920,u64::MAX),3838);
        assert_eq!(lowest_prefix_window(3000,1,1920,u64::MAX),64*1024);
        assert_eq!(lowest_prefix_window(1024*1024,500,1920,u64::MAX),4*1024*1024);
        assert_eq!(lowest_prefix_window(1920*2048,1919,1920,u64::MAX),4100);
        assert_eq!(lowest_prefix_window(u64::MAX,1,usize::MAX,u64::MAX),4*1024*1024);
        assert_eq!(lowest_prefix_window(1024*1024,500,1920,10000),10000);
    }
    #[tokio::test]
    async fn late_tile_cod_after_plt_cannot_become_a_main_geometry_packet_index() {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let marker=|code:u16,value:&[u8]| {let mut out=code.to_be_bytes().to_vec();out.extend(((value.len()+2) as u16).to_be_bytes());out.extend(value);out};
            let mut siz=vec![0,0];for n in [256u32,256,0,0,256,256,0,0] {siz.extend(n.to_be_bytes());}siz.extend([0,3,7,1,1,7,1,1,7,1,1]);
            let cod=|layers:u8|vec![1,2,0,layers,0,0,0,0,0,1,0];
            let mut data=vec![0xff,0x4f];data.extend(marker(0xff51,&siz));data.extend(marker(0xff52,&cod(2)));data.extend(marker(0xff5c,&[0x40,0x40]));
            let count=256*256*3;let mut header=vec![];
            for (z,chunk) in vec![1u8;count].chunks(65532).enumerate() {let mut value=vec![z as u8];value.extend(chunk);header.extend(marker(0xff58,&value));}
            header.extend(marker(0xff52,&cod(1)));let offset=data.len() as u64;let length=12+header.len()+2+count;
            data.extend([0xff,0x90,0,10,0,0]);data.extend((length as u32).to_be_bytes());data.extend([0,1]);data.extend(header);data.extend([0xff,0x93]);data.resize(data.len()+count,0);data.extend([0xff,0xd9]);
            let directory=tempfile::tempdir().unwrap();std::fs::write(directory.path().join("late-cod.j2k"),data).unwrap();
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());let location=Path::from("late-cod.j2k");let meta=runtime.block_on(store.head(&location)).unwrap();
            let cache=Arc::new(RangeCache {limit:64*1024*1024,state:Mutex::new(CacheState::default())});
            let (source,_)=RemoteSource::open("late-cod".into(),format!("late-cod-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),cache).unwrap();
            assert_eq!(tile_packet_count(&source.header,0),Some(393216));let mut context=ReadContext::new(None);
            let (headers,_,lengths,compatible)=source.tile_header(&TilePart {offset,length:length as u64,tile:0,ordinal:0,total:1},&mut context).unwrap();
            assert!(!compatible && lengths.is_none());assert_eq!(headers,marker(0xff52,&cod(1)));
        }).await.unwrap();
    }
    #[tokio::test] async fn queued_ranges_keep_their_network_timeout_after_admission() {
        let store=Arc::new(object_store::memory::InMemory::new());let location=Path::from("queued");
        store.put(&location,vec![7u8;1024].into()).await.unwrap();
        let profile=Arc::new(crate::read_policy::Profile::default());let mut permits=vec![];
        for _ in 0..crate::read_policy::settings().effective_source_requests() {permits.push(profile.acquire().await);}
        let client=RangeClient {budget:wire_budget::Budget::new(1),trace_id:0,object_id:"queued-test".into(),
            caller:std::panic::Location::caller(),block_size:1024,part:None,store,location,etag:None,version:None,
            network_min_us:Arc::new(AtomicU64::new(u64::MAX)),profile:profile.clone()};
        let task=tokio::spawn(async move {client.fetch_with_network_timeout(0..1024,Instant::now()+Duration::from_secs(1),None,Duration::from_millis(10)).await});
        tokio::time::timeout(Duration::from_millis(200),async {while profile.snapshot().admission_waits==0 {tokio::task::yield_now().await;}}).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;assert!(!task.is_finished());
        drop(permits);assert_eq!(task.await.unwrap().unwrap(),vec![7u8;1024]);
    }
    #[tokio::test] async fn range_body_progress_can_exceed_one_idle_interval_but_stalls_fail() {
        let chunks=futures_util::stream::iter(0..8u8).then(|n|async move {tokio::time::sleep(Duration::from_millis(10)).await;Ok::<_,anyhow::Error>([n])}).boxed();
        assert_eq!(body_with_progress_timeout(chunks,8,Duration::from_millis(40)).await.unwrap(),(0..8).collect::<Vec<u8>>());
        let stalled=futures_util::stream::once(async {tokio::time::sleep(Duration::from_millis(40)).await;Ok::<_,anyhow::Error>([1u8])}).boxed();
        assert!(body_with_progress_timeout(stalled,1,Duration::from_millis(10)).await.unwrap_err().to_string().contains("body progress"));
        let empty=futures_util::stream::iter(0..20).then(|_|async {tokio::time::sleep(Duration::from_millis(5)).await;Ok::<_,anyhow::Error>(Vec::<u8>::new())}).boxed();
        assert!(body_with_progress_timeout(empty,1,Duration::from_millis(15)).await.is_err());
    }
    #[tokio::test] async fn progressing_bodies_keep_bounds_and_an_outer_deadline() {
        let extra=futures_util::stream::iter([Ok::<_,anyhow::Error>(vec![1u8;3])]);
        assert!(body_with_progress_timeout(extra,2,Duration::from_secs(1)).await.unwrap_err().to_string().contains("exceeds requested range"));
        let short=futures_util::stream::iter([Ok::<_,anyhow::Error>(vec![1u8;2])]);
        assert!(body_with_progress_timeout(short,3,Duration::from_secs(1)).await.unwrap_err().to_string().contains("incomplete"));
        let chunks=futures_util::stream::iter(0..20u8).then(|n|async move {tokio::time::sleep(Duration::from_millis(5)).await;Ok::<_,anyhow::Error>([n])}).boxed();
        assert!(tokio::time::timeout(Duration::from_millis(20),body_with_progress_timeout(chunks,20,Duration::from_millis(50))).await.is_err());
    }
    #[test] fn prefix_leases_keep_requested_blocks_without_pinning_overfetched_gaps() {
        let mut leases=(0..16).map(|n|(n*1024,Arc::<[u8]>::from(vec![n as u8;1024]))).collect::<HashMap<_,_>>();
        let first=leases[&0].clone();let last=leases[&(15*1024)].clone();
        retain_requested_blocks(&mut leases,&[(true,1..512),(false,1024..2048),(true,15*1024+1..16*1024)],1024);
        assert_eq!(leases.len(),2);assert!(Arc::ptr_eq(&leases[&0],&first));
        assert!(Arc::ptr_eq(&leases[&(15*1024)],&last));
        assert!(leases.values().map(|b|b.len()).sum::<usize>()<8*1024);
    }
    #[test] fn wide_prefix_merges_reduce_requests_without_exceeding_byte_or_range_limits() {
        let original=(0..4096).map(|n|n*4096..n*4096+1024).collect::<Vec<_>>();
        let useful=4096*1024;let mut ordinary=original.clone();
        merge_ranges(&mut ordinary,2,4096,useful*4,4).unwrap();
        assert!(ordinary.len()>=16);
        let mut wide=original.clone();
        merge_ranges_bounded(&mut wide,2,2,useful*4,4,PREFIX_MERGE_BYTES).unwrap();
        assert_eq!(wide.len(),2);
        assert!(wide.iter().all(|r|r.end-r.start<=PREFIX_MERGE_BYTES));
        assert!(wide.iter().map(|r|r.end-r.start).sum::<u64>()<=useful*4);
        for wanted in original {assert!(wide.iter().any(|r|r.start<=wanted.start && r.end>=wanted.end));}
        let mut capped=(0..4096).map(|n|n*4096..n*4096+1024).collect::<Vec<_>>();
        assert!(merge_ranges_bounded(&mut capped,2,2,useful*2,2,PREFIX_MERGE_BYTES).is_err());
    }
    #[test] fn small_packet_batches_consider_one_read_without_forcing_overfetch_on_parallel_waves() {
        let profile=crate::read_policy::Profile::default();
        for _ in 0..32 {profile.record(131072,Duration::from_micros(2200),Duration::from_micros(2462));}
        let original=(0..12).map(|n|n*4096..n*4096+3072).collect::<Vec<_>>();
        let mut limited=original.clone();
        adaptive_ranges(&mut limited,&profile,64,128*1024,12*3072,1024*1024,|_|3).unwrap();
        let decision=profile.snapshot().last_decision.unwrap();
        assert_eq!(decision.planned_requests,1);
        assert!(decision.planned_bytes<=decision.original_bytes*2);
        assert!(decision.estimated_planned_ms<decision.estimated_original_ms*0.9);
        let mut parallel=original;
        adaptive_ranges(&mut parallel,&profile,64,128*1024,12*3072,1024*1024,|_|16).unwrap();
        assert_eq!(profile.snapshot().last_decision.unwrap().planned_requests,12,
            "same-wave requests must not be merged on an invented serial RTT saving");
    }
    #[test] fn learned_prefix_range_size_shrinks_for_a_slow_source() {
        let profile=crate::read_policy::Profile::default();
        for _ in 0..32 {profile.record(65536,Duration::from_millis(100),Duration::from_millis(200));}
        let mut ranges=(0..4096).map(|n|n*4096..n*4096+1024).collect::<Vec<_>>();
        let largest=std::cell::Cell::new(0u64);
        adaptive_ranges(&mut ranges,&profile,4096,32*1024*1024,4096*1024,PREFIX_MERGE_BYTES,|candidate|{
            if candidate.len()<4096 {largest.set(largest.get().max(candidate.iter().map(|r|r.end-r.start).max().unwrap_or(0)));}4
        }).unwrap();
        let plan=profile.snapshot().last_decision.unwrap();
        assert_eq!(plan.reason,"predictedRoundTripSavings");
        assert!(plan.planned_requests>=7,"Two8MiB reads would exceed the network time budget");
        assert!(plan.planned_requests<4096);
        // The fixture's measured body rate is64KiB/100ms. Charge the100ms
        // header once, then require every proposal to fit the8second budget.
        assert!(largest.get()>0 && 100.0+largest.get() as f64/655_360.0*1000.0<=8000.0,
            "Measured body rate plus header latency must bound every proposed transfer");
    }
    #[test] fn learned_observe_plan_handles_more_than_4096_prefixes_without_changing_reads() {
        let profile=crate::read_policy::Profile::default();
        for _ in 0..32 {profile.record(65536,Duration::from_millis(20),Duration::from_millis(21));}
        let mut ranges=(0..4459).map(|i|i*2048..i*2048+1024).collect::<Vec<_>>();
        let original=ranges.clone();let useful=ranges.len() as u64*1024;
        adaptive_ranges(&mut ranges,&profile,4459,8*1024*1024,useful,PREFIX_MERGE_BYTES,|_|4).unwrap();
        assert_eq!(ranges,original,"observe must not alter ranges");
        let decision=profile.snapshot().last_decision.unwrap();
        assert_eq!(decision.reason,"predictedRoundTripSavings");assert!(!decision.applied);
        assert!(decision.planned_requests<4459);
        assert!(decision.planned_bytes<=useful*2 && decision.planned_bytes<=8*1024*1024);
    }
    #[test]
    fn packet_marker_envelopes_preserve_lengths_and_require_eph() {
        let grids=vec![(1,1)];
        let packet=[0xff,0x91,0,4,0,7,248,78,0,0xff,0x92];
        assert_eq!(packet_length_marked(&packet,&grids,false,4,6).unwrap(),(11,9));
        for n in 0..packet.len() {assert!(packet_length_marked(&packet[..n],&grids,false,4,6).is_err());}
        assert_eq!(packet_length_marked(&packet[6..],&grids,false,4,6).unwrap(),(5,9));
        let mut bad=packet;bad[3]=5;assert!(packet_length_marked(&bad,&grids,false,4,6).is_err());
        let mut bad=packet;bad[10]=0x93;assert!(packet_length_marked(&bad,&grids,false,4,6).is_err());
        let mut empty=vec![];append_empty_packet(&mut empty,6,65537);
        assert_eq!(empty,[0xff,0x91,0,4,0,1,0,0xff,0x92]);
        assert_eq!(packet_length_marked(&empty,&grids,false,4,6).unwrap(),(9,0));
    }

    #[test]
    fn large_validated_prefix_allowance_preserves_byte_deadline_and_explicit_caps() {
        let mut context=ReadContext::new(None);context.requests_limit=1024;context.explicit_request_limit=false;
        let bytes=context.limit;let deadline=context.deadline;context.stats.requests=40;
        let scope=context.begin_index_ranges(6000);context.stats.requests+=4000;let credit=context.finish_index_ranges(scope);
        context.allow_prefix_ranges(6000,1024,credit);
        assert_eq!(context.requests_limit-context.stats.requests,16384-40);assert_eq!(context.limit,bytes);assert_eq!(context.deadline,deadline);
        context.explicit_request_limit=true;let capped=context.requests_limit;
        context.allow_prefix_ranges(u64::MAX,1024,credit);assert_eq!(context.requests_limit,capped);
    }
    #[test]
    fn marker_discovery_allowance_is_bounded_and_preserves_admin_caps() {
        let mut context=ReadContext::new(None);context.explicit_request_limit=false;context.requests_limit=5024;
        let bytes=context.limit;let deadline=context.deadline;
        context.allow_header_ranges(6000,1024,4000);assert_eq!(context.requests_limit,16000);
        context.allow_header_ranges(u64::MAX,1024,4000);assert_eq!(context.requests_limit,20384);
        assert_eq!(context.limit,bytes);assert_eq!(context.deadline,deadline);
        context.explicit_request_limit=true;context.requests_limit=512;
        context.allow_header_ranges(u64::MAX,1024,4000);assert_eq!(context.requests_limit,512);
        context.explicit_request_limit=false;context.requests_limit=5024;
        context.allow_header_ranges(960,1024,4000);assert_eq!(context.requests_limit,5024);
    }
    #[test]
    fn ordinary_prefix_requests_keep_the_existing_payload_allowance() {
        let mut context=ReadContext::new(None);context.explicit_request_limit=false;context.requests_limit=5024;
        context.allow_prefix_ranges(960,1024,4000);assert_eq!(context.requests_limit,5024);
    }
    #[test]
    fn index_allowance_preserves_remaining_payload_requests() {
        let mut context=ReadContext::new(None);context.requests_limit=1024;context.explicit_request_limit=false;
        context.stats.requests=40;let allowance=context.begin_index_ranges(6000).unwrap();assert_eq!(context.requests_limit,6256);
        context.stats.requests+=4000;context.finish_index_ranges(Some(allowance));
        assert_eq!(context.requests_limit-context.stats.requests,984);
    }
    #[test]
    fn index_allowance_honors_explicit_caps_and_small_sources() {
        let mut context=ReadContext::new(None);context.requests_limit=512;context.explicit_request_limit=true;
        assert!(context.begin_index_ranges(6000).is_none());assert_eq!(context.requests_limit,512);
        context.explicit_request_limit=false;context.requests_limit=1024;
        assert!(context.begin_index_ranges(451).is_none());assert_eq!(context.requests_limit,1024);
        let allowance=context.begin_index_ranges(u64::MAX).unwrap();assert_eq!(context.requests_limit,16384);
        context.finish_index_ranges(Some(allowance));assert_eq!(context.requests_limit,1024);
    }

    #[test]
    fn classic_segments_follow_pass_termination_and_reject_truncated_lengths() {
        let grids=vec![(1,1)];
        // Three passes, segment lengths 2/3/4. Combined flags keep TERMALL priority.
        for style in [4,5,63] {
            assert_eq!(packet_length_coding(&[248,78,0],&grids,false,style).unwrap(),(3,9));
            assert!(packet_length_coding(&[248,78],&grids,false,style).is_err());
        }
        // Thirteen BYPASS passes: ten, two, one; lengths 5/7/3.
        assert_eq!(packet_length_coding(&[254,112,174,192],&grids,false,1).unwrap(),(4,15));
        assert!(packet_length_coding(&[254,112,174],&grids,false,1).is_err());
        for style in [0,2,8,16,32,58] {
            assert_eq!(packet_length_coding(&[248,144],&grids,false,style).unwrap(),(2,9));
        }
    }

    #[test]
    fn compatibility_reuse_preserves_bytes_for_fragmented_partial_and_versioned_blocks() {
        let original=(0..61u8).collect::<Vec<_>>();
        let identity:Arc<str>=Arc::from("snapshot-a");
        let other:Arc<str>=Arc::from("snapshot-b");
        for mask in 0..256u32 {
            let cache=RangeCache {limit:1024,state:Mutex::new(CacheState::default())};
            for block in 0..8 {
                let start=block*8;let end=(start+8).min(original.len());
                if mask & (1<<block)!=0 {cache.put(&identity,8,start as u64,&original[start..end]);}
                cache.put(&other,8,start as u64,&vec![255;end-start]);
            }
            for range in [0..61,3..59,8..56,59..61,0..0] {
                let (mut bytes,missing,suffix)=cache.compatibility_edges(&identity,8,range.clone());
                assert!(missing.start>=range.start && missing.end<=range.end);
                bytes.extend_from_slice(&original[missing.start as usize..missing.end as usize]);
                bytes.extend_from_slice(&suffix);
                assert_eq!(bytes,original[range.start as usize..range.end as usize],"mask {mask}");
            }
        }
    }

    #[tokio::test]
    async fn main_component_overrides_only_share_the_default_packet_geometry_when_identical() {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let directory=tempfile::tempdir().unwrap();
            for (label,component,width,expected) in [("same",0,4,true),("different",0,5,false),("invalid-component",3,4,false)] {
                let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
                for value in [512u32,384,0,0,512,384,0,0] {data.extend_from_slice(&value.to_be_bytes());}
                data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
                data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,1,0,5,4,4,0,1]);
                let coc=[0xff,0x53,0,9,component,0,5,width,4,0,1];data.extend_from_slice(&coc);
                data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
                data.extend_from_slice(&[0xff,0x90,0,10,0,0]);data.extend_from_slice(&32782u32.to_be_bytes());
                data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(data.len()+32768,0);data.extend_from_slice(&[0xff,0xd9]);
                let filename=format!("main-coc-{label}.j2k");std::fs::write(directory.path().join(&filename),data).unwrap();
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());
                let location=Path::from(filename);let meta=runtime.block_on(store.head(&location)).unwrap();
                let (source,_)=RemoteSource::open("component-coding".into(),format!("main-coc-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),RangeCache::from_env()).unwrap();
                assert_eq!(source.header.packet_compatible,expected,"{label}");assert_eq!(source.header.prefix_compatible,expected,"{label}");
                let payload=source.acquire_header(&mut ReadContext::new(None)).unwrap();
                assert!(payload.main.windows(coc.len()).any(|bytes|bytes==coc),"component marker must be retained verbatim");
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn uncontended_fetch_fast_path_cannot_barge_a_queued_payload() {
        let gate=tokio::sync::Mutex::new(());let held=gate.lock().await;
        let queued=gate.lock();tokio::pin!(queued);
        assert!(tokio::time::timeout(Duration::from_millis(5),&mut queued).await.is_err());
        drop(held);
        assert!(gate.try_lock().is_err(),"the scanner must not steal the queued payload's permit");
        drop(queued.await);assert!(gate.try_lock().is_ok());
    }
    #[tokio::test]
    async fn cancelled_fetch_waiter_does_not_block_the_next_payload() {
        let gate=tokio::sync::Mutex::new(());let held=gate.lock().await;
        let (cancel,receiver)=watch::channel(false);
        let waiter=fetch_gate(&gate,Instant::now()+Duration::from_secs(2),Some(receiver));tokio::pin!(waiter);
        assert!(tokio::time::timeout(Duration::from_millis(5),&mut waiter).await.is_err());
        cancel.send(true).unwrap();assert!(waiter.await.unwrap_err().to_string().contains("superseded"));
        assert!(fetch_gate(&gate,Instant::now()+Duration::from_millis(5),None).await.is_err());
        drop(held);
        assert!(fetch_gate(&gate,Instant::now()+Duration::from_secs(1),None).await.is_ok());
    }
    #[test]
    fn published_prefix_requires_every_declared_part_of_each_requested_tile() {
        let selected=HashSet::from([7]);
        let part=TilePart {tile:7,offset:100,length:30,ordinal:0,total:2};
        let mut index=PartIndex {parts:vec![part.clone()],cursor:130,complete:false};
        assert!(ready_parts(&index,&selected).is_none());
        index.parts.push(TilePart {offset:130,ordinal:1,..part.clone()});
        assert_eq!(ready_parts(&index,&selected).unwrap().len(),2);
        assert!(ready_parts(&index,&HashSet::from([7,8])).is_none());
        index.parts[1].ordinal=0;assert!(ready_parts(&index,&selected).is_none());
        index.parts[1].ordinal=2;assert!(ready_parts(&index,&selected).is_none());
        index.parts[1].ordinal=1;index.parts[1].total=3;assert!(ready_parts(&index,&selected).is_none());
        for p in &mut index.parts {p.total=0;}
        assert!(ready_parts(&index,&selected).is_none());
        index.complete=true;assert_eq!(ready_parts(&index,&selected).unwrap().len(),2);
    }
    #[test]
    fn packet_leases_merge_without_double_charging_or_exceeding_request_retention() {
        let mut context = ReadContext::new(None);
        let first: Arc<[u8]> = Arc::from(vec![1;4*1024*1024]);
        context.pin_packet_blocks(HashMap::from([(0,first.clone())]));
        context.pin_packet_blocks(HashMap::from([(0,first.clone()),(4*1024*1024,first.clone())]));
        assert_eq!(context.pinned.len(),2);
        context.pin_packet_blocks(HashMap::from([(8*1024*1024,Arc::from(vec![2;1024]))]));
        assert_eq!(context.pinned.len(),2);
        assert!(Arc::ptr_eq(context.pinned.get(&0).unwrap(),&first));
    }
    #[test]
    fn distant_single_tile_payload_parallelism_keeps_other_workloads_conservative() {
        let ranges=vec![0..256*1024;14];
        assert_eq!(payload_range_concurrency(true,1024,3*1024*1024,&ranges,20000),16);
        assert_eq!(payload_range_concurrency(false,1024,3*1024*1024,&ranges,20000),4);
        assert_eq!(payload_range_concurrency(true,1024,3*1024*1024,&ranges,4999),4);
        assert_eq!(payload_range_concurrency(true,1024,3*1024*1024,&ranges,u64::MAX),4);
        assert_eq!(payload_range_concurrency(true,1024,9*1024*1024,&ranges,20000),4);
        assert_eq!(payload_range_concurrency(true,1024,3*1024*1024,&ranges[..4],20000),4);
    }
    #[test]
    fn tlm_offsets_reject_short_overflowing_and_out_of_codestream_extents() {
        assert_eq!(tlm_offsets(100,136,&[(None,14),(Some(1),20)]).unwrap(),vec![100,114]);
        assert!(tlm_offsets(100,135,&[(None,14),(None,20)]).is_err());
        assert!(tlm_offsets(100,136,&[(None,13)]).is_err());
        assert!(tlm_offsets(u64::MAX-10,u64::MAX,&[(None,14)]).is_err());
    }
    #[test]
    fn ht_packet_lengths_keep_cleanup_and_refinement_segments_separate() {
        let grids = [(1,1)];
        assert_eq!(packet_length_coding(&[0xe5],&grids,true,0x40).unwrap(),(1,5));
        assert_eq!(packet_length_coding(&[0xf2,0xb0],&grids,true,0x40).unwrap(),(2,8));
        assert_eq!(packet_length_coding(&[0xf8,0xa6],&grids,true,0x40).unwrap(),(2,8));
        assert_eq!(packet_length_coding(&[0xfa,0x28],&grids,true,0x40).unwrap(),(2,5));
        assert_ne!(packet_length_coding(&[0xf8,0xa6],&grids,false,0).unwrap(),(2,8));
        assert!(packet_length_coding(&[0xf8],&grids,true,0x40).is_err());
    }
    #[test]
    fn compact_range_parallelism_does_not_raise_bulk_transfer_admission() {
        assert_eq!(range_concurrency(1024,1024*1024,&vec![0..8192;128],2000),16);
        assert_eq!(range_concurrency(1024,1024*1024,&vec![0..8192;128],1999),4);
        assert_eq!(range_concurrency(1024,1024*1024,&vec![0..8192;128],u64::MAX),4);
        assert_eq!(range_concurrency(1024,1024*1024+1,&vec![0..8192;129],10000),4);
        assert_eq!(range_concurrency(1024,64*1024,&[0..64*1024],10000),4);
        assert_eq!(range_concurrency(256,64*1024,&[0..64*1024],0),16);
    }
    #[test]
    fn a_merged_gap_does_not_throttle_a_small_remote_batch() {
        let mut ranges=vec![0..1024;63];ranges.push(0..56*1024);
        assert_eq!(range_concurrency(1024,119*1024,&ranges,10000),16);
        assert_eq!(range_concurrency(1024,119*1024,&ranges,1999),4);
        assert_eq!(range_concurrency(1024,1024*1024,&[0..512*1024,0..512*1024],10000),4);
        assert_eq!(range_concurrency(1024,48*12*1024,&vec![0..12*1024;48],10000),16);
    }
    #[test]
    fn compatibility_cache_evicts_idle_files_and_preserves_active_decoder_leases() {
        let mut cache = CompatibilityCache::default();
        let first = Arc::new(tempfile::NamedTempFile::new().unwrap());
        let first_path = first.path().to_owned();
        cache.put("first-version".into(), first.clone(), 8, 8);
        assert!(Arc::ptr_eq(&cache.get("first-version").unwrap(), &first));
        assert!(cache.get("new-version").is_none());
        let second = Arc::new(tempfile::NamedTempFile::new().unwrap());
        let second_path = second.path().to_owned();
        cache.put("second-version".into(), second, 8, 8);
        assert_eq!(cache.bytes, 8);
        assert!(cache.get("first-version").is_none());
        assert!(first_path.exists());
        drop(first);
        assert!(!first_path.exists());
        drop(cache);
        assert!(!second_path.exists());
    }
    #[tokio::test]
    async fn retained_local_reader_rejects_changed_uncached_source_ranges() {
        let mut file=tempfile::NamedTempFile::new().unwrap();let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
        for n in [1024u32,1024,0,0,1024,1024,0,0] {data.extend(n.to_be_bytes());}
        data.extend([0,3,7,1,1,7,1,1,7,1,1]);data.extend([0xff,0x52,0,12,0,2,0,1,0,0,4,4,0,1]);data.extend([0xff,0x5c,0,4,0,0x40]);
        data.extend([0xff,0x90,0,10,0,0]);data.extend((2u32*1024*1024+14).to_be_bytes());data.extend([0,1,0xff,0x93]);data.resize(data.len()+2*1024*1024,0);data.extend([0xff,0xd9]);
        file.write_all(&data).unwrap();file.flush().unwrap();let path=file.path().to_owned();let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move||{
            let (reader,_)=RemoteSource::open_file(&path,runtime,RangeCache::from_env()).unwrap();assert_eq!(reader.local_path.as_ref(),Some(&std::fs::canonicalize(&path).unwrap()));
            let mut context=ReadContext::new(None);assert_eq!(reader.read(65536..65540,&mut context).unwrap(),vec![0;4]);
            let mut changed=std::fs::OpenOptions::new().append(true).open(&path).unwrap();changed.write_all(&[1]).unwrap();changed.flush().unwrap();
            let error=reader.read(131072..131076,&mut context).unwrap_err();let message=format!("{error:#}").to_ascii_lowercase();assert!(message.contains("precondition") || message.contains("match"),"{message}");
        }).await.unwrap();
    }
    #[tokio::test]
    async fn local_header_probe_reads_bounded_version_pinned_ranges() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0xff,0x4f,0xff,0x51,0,47,0,0];
        for value in [512u32,384,0,0,512,384,0,0] { data.extend_from_slice(&value.to_be_bytes()); }
        data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
        data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,2,0,5,4,4,0,1]);
        data.extend_from_slice(&[0xff,0x5c,0,19,0]);
        data.extend_from_slice(&[0x40;16]);
        data.extend_from_slice(&[0xff,0x90,0,10,0,0]);
        data.extend_from_slice(&(14u32+32*1024).to_be_bytes());
        data.extend_from_slice(&[0,1,0xff,0x93]);
        data.resize(data.len()+32*1024,0); data.extend_from_slice(&[0xff,0xd9]);
        file.write_all(&data).unwrap(); file.flush().unwrap();
        let path = file.path().to_owned();
        for cache in [RangeCache::from_env(),Arc::new(RangeCache {limit:0,state:Mutex::new(CacheState::default())})] {
            let path=path.clone();let runtime=tokio::runtime::Handle::current();
            let (header,stats)=tokio::task::spawn_blocking(move || RemoteSource::probe_file(&path,runtime,cache)).await.unwrap().unwrap();
            assert_eq!((header.width,header.height,header.levels,header.layers),(512,384,6,2));
            assert!(stats.bytes <= 1024 && stats.requests == 1,"header fields must reuse one fetched block even without shared retention");
        }
    }
    #[tokio::test]
    async fn initial_get_seeds_versioned_header_parsing_without_duplicate_reads() {
        let directory=tempfile::tempdir().unwrap();
        for comment_size in [0usize,2048] {
        for payload_size in [32usize,32768] {
            let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
            for value in [512u32,384,0,0,512,384,0,0] {data.extend_from_slice(&value.to_be_bytes());}
            data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
            data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,1,0,5,4,4,0,1]);
            data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
            if comment_size>0 {data.extend_from_slice(&[0xff,0x64]);data.extend_from_slice(&((comment_size+4) as u16).to_be_bytes());data.extend_from_slice(&[0,1]);data.extend(vec![b'x';comment_size]);}
            data.extend_from_slice(&[0xff,0x90,0,10,0,0]);data.extend_from_slice(&(14u32+payload_size as u32).to_be_bytes());
            data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(data.len()+payload_size,0);data.extend_from_slice(&[0xff,0xd9]);
            let filename=format!("initial-{payload_size}.j2k");std::fs::write(directory.path().join(&filename),&data).unwrap();
            for limit in [0,64*1024*1024] {
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());
                let location=Path::from(filename.clone());let probe=InitialRead::fetch(store.as_ref(),&location).await.unwrap();
                assert_eq!(probe.meta.size,data.len() as u64);assert!(probe.seed.prefix.is_some());
                let runtime=tokio::runtime::Handle::current();
                let (source,stats)=tokio::task::spawn_blocking(move || RemoteSource::open_from_probe("initial".into(),format!("initial-{}",uuid::Uuid::new_v4()),
                    store,location,probe,runtime,Arc::new(RangeCache {limit,state:Mutex::new(CacheState::default())}))).await.unwrap().unwrap();
                assert_eq!((source.header.width,source.header.height),(512,384));
                assert_eq!(stats.requests,1);assert_eq!(stats.bytes,data.len().min(INITIAL_PROBE as usize) as u64);
                assert_ne!(source.minimum_range_latency_us(),u64::MAX);
            }
        }
    }
    }
    #[tokio::test]
    async fn discovery_fetches_a_split_mandatory_sot_in_one_range() {
        let mut file=tempfile::NamedTempFile::new().unwrap();
        let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
        for value in [1024u32,512,0,0,512,512,0,0] {data.extend_from_slice(&value.to_be_bytes());}
        data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
        data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,1,0,5,4,4,0,1]);
        data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
        let first=data.len();let second=2042;
        data.extend_from_slice(&[0xff,0x90,0,10,0,0]);data.extend_from_slice(&u32::try_from(second-first).unwrap().to_be_bytes());data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(second,0);
        data.extend_from_slice(&[0xff,0x90,0,10,0,1]);data.extend_from_slice(&4110u32.to_be_bytes());data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(second+4110,0);data.extend_from_slice(&[0xff,0xd9]);
        file.write_all(&data).unwrap();file.flush().unwrap();let path=file.path().to_owned();let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            for limit in [0,64*1024*1024] {
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
                let location=Path::from(path.file_name().unwrap().to_str().unwrap());let meta=runtime.block_on(store.head(&location)).unwrap();
                let cache=Arc::new(RangeCache {limit,state:Mutex::new(CacheState::default())});
                let (source,_)=RemoteSource::open("split-sot".into(),format!("split-sot-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),cache).unwrap();
                let mut context=ReadContext::new(None);let payload=source.acquire_header(&mut context).unwrap();
                let parts=source.selected_parts(&HashSet::from([1]),&payload,2,14,false,false,&mut context).unwrap();
                assert_eq!(parts.len(),1);assert_eq!(parts[0].offset,second as u64);
                assert_eq!(context.stats.requests,if limit==0 {2}else{1},"one GET must cover both cache blocks of the mandatory header");
                assert_eq!(context.stats.bytes,if limit==0 {3072}else{2048},"coalescing must not transfer extra bytes");
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn cached_sot_discovery_does_not_extend_the_prefix_before_packet_planning() {
        let mut file=tempfile::NamedTempFile::new().unwrap();
        let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
        for value in [512u32,384,0,0,512,384,0,0] {data.extend_from_slice(&value.to_be_bytes());}
        data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
        data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,1,0,5,4,4,0,1]);
        data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
        let first_part=data.len() as u64;
        data.extend_from_slice(&[0xff,0x90,0,10,0,0]);
        data.extend_from_slice(&(14u32+32*1024).to_be_bytes());
        data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(data.len()+32*1024,0);
        data.extend_from_slice(&[0xff,0xd9]);file.write_all(&data).unwrap();file.flush().unwrap();
        let path=file.path().to_owned();let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
            let location=Path::from(path.file_name().unwrap().to_str().unwrap());
            let meta=runtime.block_on(store.head(&location)).unwrap();
            let (source,opened)=RemoteSource::open("probe-test".into(),format!("probe-{}",uuid::Uuid::new_v4()),store,location,meta,runtime,RangeCache::from_env()).unwrap();
            assert_eq!(opened.requests,1);assert_eq!(opened.bytes,1024);
            let mut context=ReadContext::new(None);let payload=source.acquire_header(&mut context).unwrap();
            let parts=source.selected_parts(&HashSet::from([0]),&payload,2048,14,false,false,&mut context).unwrap();
            assert_eq!(parts.len(),1);assert_eq!(parts[0].offset,first_part);
            assert_eq!(context.stats.requests,0);assert_eq!(context.stats.bytes,0);
            assert!(context.stats.cache_bytes>=12);
            let header=source.tile_header(&parts[0],&mut context).unwrap();
            *source.cache.state.lock().unwrap()=CacheState::default();
            context.pinned.clear();context.validated.clear();context.recent=recent_reads::RecentReads::default();
            let reads=context.stats.requests;
            assert_eq!(source.tile_header(&parts[0],&mut context).unwrap(),header);
            assert_eq!(context.stats.requests,reads,"validated request-local header must survive byte-cache eviction without another GET");
            let mut different=parts[0].clone();different.length-=1;
            assert!(source.tile_header(&different,&mut context).unwrap_err().to_string().contains("disagrees with source SOT"),"header reuse must validate the complete tile-part identity");
        }).await.unwrap();
    }
    #[tokio::test]
    async fn large_plt_headers_preserve_bounds_with_and_without_shared_cache() {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let directory=tempfile::tempdir().unwrap();
            let count=196608usize;
            let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
            for value in [4096u32,4096,0,0,4096,4096,0,0] {data.extend_from_slice(&value.to_be_bytes());}
            data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
            data.extend_from_slice(&[0xff,0x52,0,13,1,2,0,1,0,0,2,2,0,1,0x44]);
            data.extend_from_slice(&[0xff,0x5c,0,4,0,0x40]);
            let offset=data.len() as u64;
            let mut markers=vec![];
            for (index,chunk) in vec![1u8;count].chunks(65532).enumerate() {
                markers.extend_from_slice(&[0xff,0x58]);
                markers.extend_from_slice(&((chunk.len()+3) as u16).to_be_bytes());
                markers.push(index as u8);markers.extend_from_slice(chunk);
            }
            let length=(12+markers.len()+2+count) as u32;
            data.extend_from_slice(&[0xff,0x90,0,10,0,0]);data.extend_from_slice(&length.to_be_bytes());data.extend_from_slice(&[0,1]);
            data.extend(markers);data.extend_from_slice(&[0xff,0x93]);let payload=data.len() as u64;
            data.resize(data.len()+count,0xa5);data.extend_from_slice(&[0xff,0xd9]);
            std::fs::write(directory.path().join("large-plt.j2k"),data).unwrap();
            for limit in [0,64*1024*1024] {
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());
                let location=Path::from("large-plt.j2k");let meta=runtime.block_on(store.head(&location)).unwrap();
                let cache=Arc::new(RangeCache {limit,state:Mutex::new(CacheState::default())});
                let (source,_)=RemoteSource::open("plt-batch".into(),format!("plt-batch-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),cache).unwrap();
                let mut context=ReadContext::new(None);let part=TilePart {offset,length:u64::from(length),tile:0,ordinal:0,total:1};
                let (_,start,lengths,compatible)=source.tile_header(&part,&mut context).unwrap();
                assert!(compatible);assert_eq!(start,payload);assert_eq!(*lengths.unwrap(),vec![1u64;count]);
                assert!(context.stats.requests<=4,"PLT metadata exceeded its bounded read count: {}",context.stats.requests);
                assert!(context.stats.bytes<=payload.div_ceil(BLOCK)*BLOCK,"batch crossed required metadata blocks");
                assert!(context.pinned.values().map(|b|b.len()).sum::<usize>()<=8*1024*1024);
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn parsed_multitile_headers_skip_discovery_when_byte_cache_is_disabled() {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let directory=tempfile::tempdir().unwrap();
            let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
            for value in [128u32,64,0,0,64,64,0,0] {data.extend_from_slice(&value.to_be_bytes());}
            data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
            data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,1,0,0,4,4,0,1]);
            data.extend_from_slice(&[0xff,0x5c,0,4,0,0x40]);
            data.extend_from_slice(&[0xff,0x55,0,16,0,0x60]);
            for tile in 0..2u16 {data.extend_from_slice(&tile.to_be_bytes());data.extend_from_slice(&65536u32.to_be_bytes());}
            let mut parts=vec![];
            for tile in 0..2u16 {
                let offset=data.len() as u64;let length=65536u32;
                data.extend_from_slice(&[0xff,0x90,0,10]);data.extend_from_slice(&tile.to_be_bytes());
                data.extend_from_slice(&length.to_be_bytes());data.extend_from_slice(&[0,1]);
                data.extend_from_slice(&[0xff,0x58,0,6,0,1,1,1,0xff,0x93]);
                data.resize(offset as usize+length as usize,0);
                parts.push(TilePart {tile,offset,length:u64::from(length),ordinal:0,total:1});
            }
            data.extend_from_slice(&[0xff,0xd9]);std::fs::write(directory.path().join("headers.j2k"),data).unwrap();
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());
            let location=Path::from("headers.j2k");let meta=runtime.block_on(store.head(&location)).unwrap();
            let cache=Arc::new(RangeCache {limit:0,state:Mutex::new(CacheState::default())});
            let (source,_)=RemoteSource::open("parsed-headers".into(),format!("parsed-{}",uuid::Uuid::new_v4()),store,location,meta,runtime,cache).unwrap();
            let mut warm=ReadContext::new(None);
            let payload=source.acquire_header(&mut warm).unwrap();
            for part in &parts {assert_eq!(source.tile_header(part,&mut warm).unwrap().2.as_deref().unwrap(),&vec![1,1,1]);}
            for metadata_limit in [0,64*1024] {
                let mut next=ReadContext::new(None);next.metadata_limit=metadata_limit;
                let selected=source.selected_parts(&HashSet::from([0,1]),&payload,2,14,false,false,&mut next).unwrap();
                assert_eq!(selected,parts);
                source.prefetch_tile_headers(&parts,4,&mut next).unwrap();
                for part in &parts {assert!(source.tile_header(part,&mut next).unwrap().3);}
                assert_eq!(next.stats.requests,0,"parsed metadata must bypass speculative header discovery, even without byte caching");
                assert_eq!(next.stats.bytes,0);assert!(next.metadata_bytes<=metadata_limit);
                let mut changed=parts[1].clone();changed.length-=1;
                assert!(source.tile_header(&changed,&mut next).is_err(),"a changed extent must still validate its SOT");
            }
            for (probe,complete) in [(64,false),(14,true)] {
                let mut next=ReadContext::new(None);
                assert_eq!(source.selected_parts(&HashSet::from([0,1]),&payload,2,probe,complete,false,&mut next).unwrap(),parts);
                assert!(next.stats.requests>0,"parsed metadata must not suppress requested payload prefetch");
                if complete {assert!(next.stats.bytes>=2*65536);}
            }
            let mut wrong=(*payload).clone();wrong.tlm[0].1-=1;
            let mut invalid=ReadContext::new(None);
            assert!(source.selected_parts(&HashSet::from([0]),&wrong,2,14,false,false,&mut invalid)
                .unwrap_err().to_string().contains("TLM does not match SOT"));
        }).await.unwrap();
    }
    #[tokio::test]
    async fn indexed_header_batches_are_not_reread_without_a_shared_cache() {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let directory=tempfile::tempdir().unwrap();
            for style in [0x40u8,0x60] {
                let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
                for value in [512u32,384,0,0,64,64,0,0] {data.extend_from_slice(&value.to_be_bytes());}
                data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
                data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,1,0,5,4,4,0,1]);
                data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
                let entries=(0..48u16).flat_map(|tile| {
                    let mut entry=vec![];if style==0x60 {entry.extend_from_slice(&tile.to_be_bytes());}
                    entry.extend_from_slice(&4096u32.to_be_bytes());entry
                }).collect::<Vec<_>>();
                data.extend_from_slice(&[0xff,0x55]);data.extend_from_slice(&(entries.len() as u16+4).to_be_bytes());data.extend_from_slice(&[0,style]);data.extend(entries);
                for tile in 0..48u16 {
                    data.extend_from_slice(&[0xff,0x90,0,10]);data.extend_from_slice(&tile.to_be_bytes());data.extend_from_slice(&4096u32.to_be_bytes());data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(data.len()+4096-14,0);
                }
                data.extend_from_slice(&[0xff,0xd9]);let name=format!("headers-{style}.j2k");std::fs::write(directory.path().join(&name),data).unwrap();
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());
                let location=Path::from(name);let meta=runtime.block_on(store.head(&location)).unwrap();
                let cache=Arc::new(RangeCache {limit:0,state:Mutex::new(CacheState::default())});
                let (source,_)=RemoteSource::open("headers".into(),format!("headers-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),cache).unwrap();
                let mut context=ReadContext::new(None);let payload=source.acquire_header(&mut context).unwrap();
                let selected=(0..48).collect::<HashSet<u16>>();let parts=source.selected_parts(&selected,&payload,2,14,false,false,&mut context).unwrap();
                assert_eq!(parts.len(),48);assert_eq!(context.stats.requests,48,"each indexed SOT block must be fetched only once, including anonymous TLM batches");
                assert_eq!(context.pinned.len(),48);
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn serial_index_admission_follows_actual_tlm_headers_across_quality_models() {
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let directory=tempfile::tempdir().unwrap();
            for (layers,style,serial,prefix) in [(1u8,None,true,true),(2,None,true,false),
                (2,Some(0x40u8),true,false),(2,Some(0x60u8),false,false)] {
                let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
                for value in [512u32,384,0,0,512,384,0,0] {data.extend_from_slice(&value.to_be_bytes());}
                data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
                data.extend_from_slice(&[0xff,0x52,0,12,0,0,0,layers,0,5,4,4,0,1]);
                data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
                let length=14u32+32768;
                if let Some(style)=style {
                    data.extend_from_slice(&[0xff,0x55,0,if style==0x60 {10}else{8},0,style]);
                    if style==0x60 {data.extend_from_slice(&[0,0]);}
                    data.extend_from_slice(&length.to_be_bytes());
                }
                data.extend_from_slice(&[0xff,0x90,0,10,0,0]);data.extend_from_slice(&length.to_be_bytes());
                data.extend_from_slice(&[0,1,0xff,0x93]);data.resize(data.len()+32768,0);data.extend_from_slice(&[0xff,0xd9]);
                let filename=format!("admission-{layers}-{style:?}.j2k").replace(':',"-");
                std::fs::write(directory.path().join(&filename),data).unwrap();
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());
                let location=Path::from(filename);let meta=runtime.block_on(store.head(&location)).unwrap();
                let (source,_)=RemoteSource::open("admission".into(),format!("admission-{}",uuid::Uuid::new_v4()),
                    store,location,meta,runtime.clone(),RangeCache::from_env()).unwrap();
                assert_eq!(source.header.layers,u16::from(layers));
                assert_eq!(source.serial_index,serial,"TLM {style:?}, layers {layers}");
                assert_eq!(source.sequential_prefix,prefix,"prefix policy must remain independent");
            }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn contended_ordinary_grid_skips_speculative_source_io() {
        let mut file=tempfile::NamedTempFile::new().unwrap();
        let mut data=vec![0xff,0x4f,0xff,0x51,0,47,0,0];
        for value in [65536u32,512,0,0,512,512,0,0] {data.extend_from_slice(&value.to_be_bytes());}
        data.extend_from_slice(&[0,3,7,1,1,7,1,1,7,1,1]);
        data.extend_from_slice(&[0xff,0x52,0,12,0,2,0,1,1,5,4,4,0,1]);
        data.extend_from_slice(&[0xff,0x5c,0,19,0]);data.extend_from_slice(&[0x40;16]);
        let size=64*1024*1024u64;let first=data.len() as u64;
        data.extend_from_slice(&[0xff,0x90,0,10,0,0]);data.extend_from_slice(&u32::try_from(size-first-2).unwrap().to_be_bytes());data.extend_from_slice(&[0,1,0xff,0x93]);
        file.write_all(&data).unwrap();file.as_file().set_len(size).unwrap();
        std::io::Seek::seek(file.as_file_mut(),std::io::SeekFrom::Start(size-2)).unwrap();file.write_all(&[0xff,0xd9]).unwrap();file.flush().unwrap();
        let path=file.path().to_owned();let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move||{
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
            let location=Path::from(path.file_name().unwrap().to_str().unwrap());let meta=runtime.block_on(store.head(&location)).unwrap();
            let (source,_)=RemoteSource::open("contention-test".into(),format!("contention-{}",uuid::Uuid::new_v4()),store,location,meta,runtime,RangeCache::from_env()).unwrap();
            source.network_min_us.store(2000,Ordering::Relaxed);
            let selected=(0..128).collect();let index=PartIndex {parts:vec![],cursor:first,complete:false};
            let mut context=ReadContext::new(None);let mut attempted=false;
            let result={
                let _preparations=(0..crate::read_policy::settings().effective_source_requests())
                    .map(|_|source.read_profile.preparation()).collect::<Vec<_>>();
                source.index_lanes(&index,&selected,true,&mut attempted,&mut context).unwrap()
            };
            assert!(attempted && result.is_some_and(|lanes|lanes.is_empty()));
            assert_eq!((context.stats.requests,context.stats.bytes),(0,0),"contention must not fetch even a pilot range");
            let other=crate::read_policy::Profile::default();
            let _other_work=(0..32).map(|_|other.preparation()).collect::<Vec<_>>();
            let mut attempted=false;
            source.index_lanes(&index,&selected,true,&mut attempted,&mut context).unwrap();
            assert!(attempted && context.stats.requests>0,"unrelated source activity must preserve this source's probe opportunity");
        }).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Requires the hash-pinned local organizational-style diagnostic fixture"]
    async fn local_rpcl_plt_stream_plan_matches_original_pixels() {
        use sha2::{Digest,Sha256};
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/organizational-multipart-v1/singlepart-plt-m345.jp2");
        assert_eq!(hex::encode(Sha256::digest(std::fs::read(&path).unwrap())),"9a59fb633bcf75a1047dcd5d8f5909496f1af542a23234496f0a4f988da2c25b");
        let bytes=std::fs::read(path.with_file_name("openjpeg-plt.jp2")).unwrap();
        assert_eq!(hex::encode(Sha256::digest(&bytes)),"b4a232b8b6c5fa815dae40990fea2f194fd40c02bbdd932492fd106e47c8f3a4");
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move||{
            for layers in [0,1,5,10] {
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
                let location=Path::from("singlepart-plt-m345.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
                let (source,_)=RemoteSource::open("local-plt-test".into(),format!("plt-test-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),RangeCache::from_env()).unwrap();
                assert_eq!(source.header.progression,2);assert_eq!(source.header.layers,10);
                let tiles=(0..source.header.height.div_ceil(512)).flat_map(|y|(0..source.header.width.div_ceil(512)).map(move|x|(x,y))).collect::<Vec<_>>();
                let mut context=ReadContext::new(None);
                let prepared=source.prepare(&tiles,512,6,layers,true,true,&mut context).unwrap();
                assert_eq!(prepared.route,"s3-packet-ranges");assert!(context.orders.is_empty(),"RPCL must not materialize packet-order vectors");
                let params=jpeg2k::DecodeParameters::new().reduce(6).layers(layers);
                let original=jpeg2k::Image::from_bytes_with(&bytes,params.clone()).unwrap();
                let subset=jpeg2k::Image::from_bytes_with(&prepared.bytes,params).unwrap();
                assert_eq!(original.components().len(),subset.components().len());
                for (a,b) in original.components().iter().zip(subset.components()) {
                    assert_eq!((a.width(),a.height()),(b.width(),b.height()));assert_eq!(a.data(),b.data(),"quality layers {layers}");
                }
                eprintln!("local PLT layers={layers} reads={} bytes={} exact=true",context.stats.requests,context.stats.bytes);
            }
        }).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Requires independently validated sparse fine-precinct diagnostic"]
    async fn compact_prefix_placeholder_budget_probe() {
        use sha2::{Digest,Sha256};use std::io::Read;
        let folder=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/empty-fine-m356");
        let proof:serde_json::Value=serde_json::from_slice(&std::fs::read(folder.join("fixture.json")).unwrap()).unwrap();assert_eq!(proof["independentlyValidated"],true);
        let mut f=std::fs::File::open(folder.join("empty-fine.j2k")).unwrap();let mut hash=Sha256::new();let mut buffer=vec![0;1024*1024];
        loop {let n=f.read(&mut buffer).unwrap();if n==0 {break;}hash.update(&buffer[..n]);}
        assert_eq!(hex::encode(hash.finalize()),"11c264a6065f5da5e01e8ac9348616278554c4ec47a25fe92cc8d63f56d121ad");
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move||{
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(&folder).unwrap());
            let location=Path::from("empty-fine.j2k");let meta=runtime.block_on(store.head(&location)).unwrap();
            let (source,opened)=RemoteSource::open("placeholder-probe".into(),format!("placeholder-{}",uuid::Uuid::new_v4()),store,location,meta,runtime,RangeCache::from_env()).unwrap();
            for reduction in [14,5] {
                let mut context=ReadContext::new(None);let payload=source.acquire_header(&mut context).unwrap();
                let view=source.prepare_multipart_prefix_enabled(&HashSet::from([0]),&payload,reduction,0,&mut context).unwrap();
                assert!(view.is_some(),"compact prefixes must not budget omitted high-band placeholders");
                if let Some(v)=&view {
                    let image=jpeg2k::Image::from_bytes_with(&v.bytes,jpeg2k::DecodeParameters::new().reduce(u32::from(reduction-v.view_reduction))).unwrap();
                    let size=16384>>reduction;assert_eq!(image.components().len(),3);
                    for c in image.components(){assert_eq!((c.width(),c.height()),(size,size));assert!(c.data().iter().all(|&v|v==128));}
                }
                eprintln!("PLACEHOLDER_PROBE {}",serde_json::json!({"reduction":reduction,"selectiveView":view.is_some(),"openedBytes":opened.bytes,"prepareReadBytes":context.stats.bytes,"localReads":context.stats.requests,"totalPackets":tile_packet_count(&source.header,0),"prefixPackets":multipart_prefix::counts(&source.header,0,source.header.levels-1-reduction).unwrap().0}));
            }
        }).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Scaled metadata-pressure diagnostic on a hash-pinned valid multipart file"]
    async fn multipart_prefix_metadata_budget_probe() {
        use sha2::{Digest,Sha256};
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/organizational-multipart-v1/openjpeg-plt.jp2");
        let bytes=std::fs::read(&path).unwrap();assert_eq!(hex::encode(Sha256::digest(&bytes)),"b4a232b8b6c5fa815dae40990fea2f194fd40c02bbdd932492fd106e47c8f3a4");
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move||{
            for limit in [96,128,160,192,256] {
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
                let location=Path::from("openjpeg-plt.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
                let (source,_)=RemoteSource::open("metadata-probe".into(),format!("metadata-probe-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),RangeCache::from_env()).unwrap();
                let mut context=ReadContext::new(None);context.metadata_limit=limit*1024;
                let payload=source.acquire_header(&mut context).unwrap();let selected=(0..source.header.layout.columns*source.header.layout.rows).map(|n|n as u16).collect();
                let prepared=source.prepare_multipart_prefix_enabled(&selected,&payload,5,0,&mut context).unwrap();
                assert!(prepared.is_some(),"valid indexed prefix must fit the scaled {limit}KiB metadata budget");
                if let Some(view)=&prepared {
                    for layers in [0,1,5,10] {
                        let original=jpeg2k::Image::from_bytes_with(&bytes,jpeg2k::DecodeParameters::new().reduce(5).layers(layers)).unwrap();
                        let actual=jpeg2k::Image::from_bytes_with(&view.bytes,jpeg2k::DecodeParameters::new().reduce(5-u32::from(view.view_reduction)).layers(layers)).unwrap();
                        assert_eq!(original.components().len(),actual.components().len());
                        for (a,b) in original.components().iter().zip(actual.components()) {assert_eq!((a.width(),a.height()),(b.width(),b.height()));assert_eq!(a.data(),b.data());}
                    }
                }
                eprintln!("METADATA_PROBE {}",serde_json::json!({"limitBytes":context.metadata_limit,"accountedBytes":context.metadata_bytes,"selectiveView":prepared.is_some(),"localReads":context.stats.requests,"sourceBytes":context.stats.bytes,"pixelsExactWhenProduced":true}));
            }
        }).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Hash-pinned synthetic ten-layer ROI; explicit isolated process only"]
    async fn multilayer_multipart_spatial_roi_matches_full_decode() {
        use sha2::{Digest,Sha256};
        assert!(std::env::var("RIVER_S3_PRECINCT_ROI").is_ok_and(|v|v=="true"),"run this isolated probe with RIVER_S3_PRECINCT_ROI=true");
        assert!(std::env::var("RIVER_S3_EXACT_SUPPORT").is_ok_and(|v|v=="true"));
        assert!(std::env::var("RIVER_S3_PRECINCT_REBASE").is_ok_and(|v|v=="true"));
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/simd-m636/preparation-m676/openjpeg.jp2");
        let bytes=std::fs::read(&path).unwrap();
        assert_eq!(hex::encode(Sha256::digest(&bytes)),"b64add81933815908b96457631673c93d037136e7918b22ca12032f2958b440c");
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
            let location=Path::from("openjpeg.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
            let cache=Arc::new(RangeCache {limit:0,state:Mutex::new(CacheState::default())});
            let (source,_)=RemoteSource::open("multilayer-roi".into(),format!("multilayer-roi-{}",uuid::Uuid::new_v4()),store,location,meta,runtime,cache).unwrap();
            assert_eq!((source.header.layers,source.header.progression),(10,2));
            for reduction in [0,2] {for layers in [0,1,5,10] {
                let params=jpeg2k::DecodeParameters::new().reduce(reduction).layers(layers);
                let original=jpeg2k::Image::from_bytes_with(&bytes,params.clone()).unwrap();
                let last=7u32>>reduction;
                for (x,y) in [(0,0),(last,0),(last,last)] {
                    let mut context=ReadContext::new(None);let payload=source.acquire_header(&mut context).unwrap();
                    let roi=precinct_roi::Selection::new(&source.header,&[(x,y)],256<<reduction,reduction as u8,&mut context)
                        .expect("multilayer spatial selection must be available");
                    assert!(roi.rebase(&source.header).is_none(),"multilayer coordinates must remain unchanged");
                    let parts=source.selected_parts(&HashSet::from([0]),&payload,2,14,false,false,&mut context).unwrap();
                    let prepared=source.prepare_indexed_multipart(&parts,&payload,reduction as u8,layers,Some(&roi),&mut context).unwrap().unwrap();
                    assert!(context.metadata_bytes<=context.metadata_limit,"batch scratch accounting must be released within the request budget");
                    let subset=jpeg2k::Image::from_bytes_with(&prepared.bytes,params.clone()).unwrap();
                    for (expected,actual) in original.components().iter().zip(subset.components()) {
                        assert_eq!((expected.width(),expected.height()),(actual.width(),actual.height()));
                        let stride=expected.width() as usize;
                        for row in y as usize*256..(y as usize+1)*256 {
                            let start=row*stride+x as usize*256;
                            assert_eq!(&expected.data()[start..start+256],&actual.data()[start..start+256],"r={reduction} layers={layers} tile={x},{y}");
                        }
                    }
                    eprintln!("MULTILAYER_ROI {}",serde_json::json!({"reduction":reduction,"layers":layers,"x":x,"y":y,"reads":context.stats.requests,"bytes":context.stats.bytes,"exact":true}));
                }
            }}
        }).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Isolated same-source multipart ROI byte accounting, not HTTP timing"]
    async fn multilayer_multipart_roi_io_comparison() {
        use sha2::{Digest,Sha256};
        assert_eq!(std::env::var("RIVER_S3_PRECINCT_ROI").unwrap(),"true");
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/simd-m636/preparation-m676/openjpeg.jp2");
        assert_eq!(hex::encode(Sha256::digest(std::fs::read(&path).unwrap())),"b64add81933815908b96457631673c93d037136e7918b22ca12032f2958b440c");
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
            let location=Path::from("openjpeg.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
            let cache=Arc::new(RangeCache {limit:0,state:Mutex::new(CacheState::default())});
            let (source,_)=RemoteSource::open("multilayer-io".into(),format!("multilayer-io-{}",uuid::Uuid::new_v4()),store,location,meta,runtime,cache).unwrap();
            let mut warm=ReadContext::new(None);let payload=source.acquire_header(&mut warm).unwrap();
            let parts=source.selected_parts(&HashSet::from([0]),&payload,2,14,false,false,&mut warm).unwrap();
            for part in &parts {source.tile_header(part,&mut warm).unwrap();}
            let mut baseline=ReadContext::new(None);
            let full=source.prepare_indexed_multipart(&parts,&payload,0,0,None,&mut baseline).unwrap().unwrap();
            let mut selective=ReadContext::new(None);
            let roi=precinct_roi::Selection::new(&source.header,&[(0,0)],256,0,&mut selective).unwrap();
            let window=source.prepare_indexed_multipart(&parts,&payload,0,0,Some(&roi),&mut selective).unwrap().unwrap();
            assert!(selective.stats.bytes*4<baseline.stats.bytes);
            assert!(window.bytes.len()*4<full.bytes.len());
            eprintln!("MULTILAYER_IO_AB {}",serde_json::json!({"scope":"same source, parsed metadata warm, byte cache disabled; full-quality256px corner tile; logical local reads, not physical HTTP GETs or latency", "sourceBytes":source.meta.size,"baselineBytes":baseline.stats.bytes,"selectiveBytes":selective.stats.bytes,"baselineReads":baseline.stats.requests,"selectiveReads":selective.stats.requests,"baselineSubsetBytes":full.bytes.len(),"selectiveSubsetBytes":window.bytes.len()}));
        }).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "Requires the hash-pinned local multipart PLT fixture"]
    async fn local_multipart_temporary_order_matches_original_pixels() {
        multipart_fixture_samples(false).await;
    }
    #[tokio::test]
    #[ignore = "Release-mode local multipart preparation A/B; not S3 latency"]
    async fn local_multipart_rpcl_preparation_benchmark() {
        multipart_fixture_samples(true).await;
    }
    async fn multipart_fixture_samples(benchmark:bool) {
        use sha2::{Digest,Sha256};
        let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/organizational-multipart-v1/openjpeg-plt.jp2");
        let bytes=std::fs::read(&path).unwrap();
        assert_eq!(hex::encode(Sha256::digest(&bytes)),"b4a232b8b6c5fa815dae40990fea2f194fd40c02bbdd932492fd106e47c8f3a4");
        let runtime=tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move||{
            let cases=if benchmark {(0..6).flat_map(|pair| {
                (if pair%2==0 {[true,false]}else{[false,true]}).map(move|legacy|(pair,0,legacy))
            }).collect::<Vec<_>>()}else{[0,1,5,10].into_iter().map(|layers|(0,layers,false)).collect()};
            for (pair,layers,legacy) in cases {
                let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
                let location=Path::from("openjpeg-plt.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
                let (source,_)=RemoteSource::open("multipart-test".into(),format!("multipart-test-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),RangeCache::from_env()).unwrap();
                assert_eq!(source.header.progression,2);assert_eq!(source.header.layers,10);
                let started=Instant::now();
                let mut context=ReadContext::new(None);context.legacy_multipart_plan=legacy;
                let payload=source.acquire_header(&mut context).unwrap();
                let selected=(0..source.header.layout.columns*source.header.layout.rows).map(|n|u16::try_from(n).unwrap()).collect();
                let parts=source.selected_parts(&selected,&payload,2,14,false,true,&mut context).unwrap();
                assert!(parts.iter().any(|p|p.ordinal>0));
                let prepared=source.prepare_indexed_multipart(&parts,&payload,6,layers,None,&mut context).unwrap().unwrap();
                let prepare_ms=started.elapsed().as_secs_f64()*1000.0;
                assert_eq!(prepared.route,"s3-multipart-packet-ranges");if !legacy {assert!(context.orders.is_empty());}
                let params=jpeg2k::DecodeParameters::new().reduce(6).layers(layers);
                let original=jpeg2k::Image::from_bytes_with(&bytes,params.clone()).unwrap();
                let subset=jpeg2k::Image::from_bytes_with(&prepared.bytes,params).unwrap();
                assert_eq!(original.components().len(),subset.components().len());
                for (a,b) in original.components().iter().zip(subset.components()) {
                    assert_eq!((a.width(),a.height()),(b.width(),b.height()));assert_eq!(a.data(),b.data(),"quality layers {layers}");
                }
                let order_bytes=context.orders.values().map(|(packets,_)|packets.capacity()*std::mem::size_of::<Packet>()).sum::<usize>();
                eprintln!("MULTIPART_SAMPLE {}",serde_json::json!({"pair":pair,"legacy":legacy,"layers":layers,"localReads":context.stats.requests,"bytes":context.stats.bytes,"prepareMs":prepare_ms,"retainedPacketOrderBytes":order_bytes,"exact":true}));
            }
        }).await.unwrap();
    }
    #[test]
    #[ignore = "Isolated release-mode RPCL planner benchmark, not server latency"]
    fn rpcl_plt_planner_benchmark() {
        let h=Header {image_origin:(0,0),width:1024,height:1024,components:3,bit_depth:16,
            levels:9,layers:10,layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:1,rows:1},
            progression:2,is_ht:false,cod:vec![1,2,0,10,1,8,4,4,4,1,0x33,0x33,0x33,0x33,0x33,0x33,0x33,0x33,0x33],
            first_part:100,codestream_end:100_000_000,packet_compatible:true,prefix_compatible:false,payload_digest:[0;32]};
        let build=|stream:bool| {
            let mut spans=Vec::new();let mut offset=100u64;
            let mut append=|p:&Packet,n:usize| {let length=(n%31+1) as u64;spans.push((p.resolution<=2 && p.layer<5,offset..offset+length));offset+=length;Ok(())};
            if stream {visit_rpcl_packets(&h,0,&mut append).unwrap();}
            else {let (packets,_)=packet_order(&h,0,None).unwrap();for (n,p) in packets.iter().enumerate(){append(p,n).unwrap();}}
            spans
        };
        let expected=build(false);assert_eq!(build(true),expected);let count=expected.len();drop(expected);
        for pair in 0..6 {for stream in if pair%2==0 {[false,true]}else{[true,false]} {
            let started=Instant::now();let mut checksum=0u64;
            for _ in 0..4 {let spans=std::hint::black_box(build(stream));checksum+=spans.len() as u64+spans.last().unwrap().1.end;}
            eprintln!("RPCL_BENCH {}",serde_json::json!({"pair":pair,"stream":stream,"ns":started.elapsed().as_nanos(),"packets":count,"descriptorBytesAvoided":if stream {count*std::mem::size_of::<Packet>()}else{0},"checksum":checksum}));
        }}
    }
    #[test]
    fn compact_prefix_plan_preserves_packet_order_and_omitted_count() {
        for progression in 0..=2 {
            let layers = if progression == 0 { 3 } else { 1 };
            let mut header = Header { image_origin: (0,0), width: 1024, height: 1024, components: 3, bit_depth: 8,
                levels: 5, layers, layout: NativeTileLayoutHint { origin_x: 0, origin_y: 0,
                    width: 1024, height: 1024, columns: 1, rows: 1 }, progression, is_ht: false,
                cod: vec![1,progression,0,layers as u8,1,4,3,3,0,1,0x44,0x44,0x44,0x44,0x44],
                first_part: 100, codestream_end: 1_000_000, packet_compatible: true,
                prefix_compatible: true, payload_digest: [0;32] };
            for edge in [false, true] {
            let tile = if edge {
                header.width = 1001; header.height = 777;
                header.layout.width = 512; header.layout.height = 512;
                header.layout.columns = 2; header.layout.rows = 2;
                header.cod[10..].copy_from_slice(&[0x33,0x44,0x55,0x44,0x33]);
                3
            } else { 0 };
            let (full, total) = packet_order(&header, tile, None).unwrap();
            if progression==2 {for layers in [1,3,10] {let mut candidate=header.clone();candidate.layers=layers;let mut streamed=vec![];
                let count=visit_rpcl_packets(&candidate,tile,|p,n| {assert_eq!(n,streamed.len());streamed.push(p.clone());Ok(())}).unwrap();
                let (materialized,expected)=packet_order(&candidate,tile,None).unwrap();assert_eq!(streamed,materialized);assert_eq!(count,expected);
            }}
            assert_eq!(tile_packet_count(&header,tile),Some(total as u64));
            if progression == 0 && !edge {
                let started = Instant::now();
                for _ in 0..32 { std::hint::black_box(packet_order(&header, tile, None).unwrap()); }
                let full_ms = started.elapsed().as_secs_f64()*1000.0;
                let started = Instant::now();
                for _ in 0..32 { std::hint::black_box(packet_order(&header, tile, Some(0)).unwrap()); }
                let prefix_ms = started.elapsed().as_secs_f64()*1000.0;
                let (prefix, _) = packet_order(&header, tile, Some(0)).unwrap();
                eprintln!("32 prefix plans: full {full_ms:.3} ms / {} descriptor bytes; compact {prefix_ms:.3} ms / {} descriptor bytes",
                    full.capacity()*std::mem::size_of::<Packet>(), prefix.capacity()*std::mem::size_of::<Packet>());
            }
            for wanted in 0..4 {
                let (prefix, counted) = packet_order(&header, tile, Some(wanted)).unwrap();
                assert_eq!(counted, total);
                let expected: Vec<_> = full.iter().take_while(|p| p.layer == 0 && p.resolution <= wanted).cloned().collect();
                assert_eq!(prefix, expected);
                if wanted == 0 && !edge { assert!(prefix.len()*100 < full.len()); }
            }
            }
            header.levels = 1;
            header.cod.truncate(11); header.cod[10] = 0;
            header.width = u32::MAX; header.height = u32::MAX;
            header.layout.width = u32::MAX; header.layout.height = u32::MAX;
            header.layout.columns = 1; header.layout.rows = 1;
            header.layers = u16::MAX;
            for prefix in [None, Some(0)] {
                assert!(packet_order(&header, 0, prefix).unwrap_err().to_string().contains("overflow"));
            }
            header.width = 4096; header.height = 4096; header.layers = 1;
            assert!(packet_order(&header, 0, Some(0)).unwrap_err().to_string().contains("memory budget"));
        }
    }
    #[test]
    fn heap_range_planner_matches_scan_planner() {
        fn reference(ranges: &mut Vec<Range<u64>>, target: usize, budget: u64) {
            let mut total: u64 = ranges.iter().map(|r| r.end - r.start).sum();
            while ranges.len() > target {
                let nearest = ranges.windows(2).enumerate()
                    .filter(|(_, pair)| pair[1].end - pair[0].start <= 1024 * 1024)
                    .min_by_key(|(_, pair)| pair[1].start - pair[0].end).map(|(i, _)| i);
                let Some(i) = nearest else { break; };
                let extra = ranges[i + 1].start - ranges[i].end;
                if total + extra > budget { break; }
                total += extra; ranges[i].end = ranges[i + 1].end; ranges.remove(i + 1);
            }
        }
        let mut seed = 7u64;
        for case in 0..200 {
            let mut cursor = 0;
            let mut actual = vec![];
            for _ in 0..(case + 2) {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                cursor += (seed >> 32) % 80_000;
                let length = 256 + seed % 32_768;
                actual.push(cursor..cursor + length); cursor += length;
            }
            let useful: u64 = actual.iter().map(|r| r.end - r.start).sum();
            let mut expected = actual.clone();
            let target = 1 + case % 48;
            reference(&mut expected, target, useful * 2);
            merge_ranges(&mut actual, target, 1000, u64::MAX, 2).unwrap();
            assert_eq!(actual, expected, "case {case}");
        }
        let input: Vec<_> = (0..10_000u64).map(|i| i * 512..i * 512 + 256).collect();
        let mut expected = input.clone();
        let started = Instant::now();
        reference(&mut expected, 48, 5_120_000);
        let scan_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut actual = input;
        let started = Instant::now();
        merge_ranges(&mut actual, 48, 1000, u64::MAX, 2).unwrap();
        let heap_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(actual, expected);
        eprintln!("10,000 dense ranges: scan {scan_ms:.2} ms; heap {heap_ms:.2} ms");
    }
    #[test] fn component_overrides_require_identical_geometry_and_coding_style() {
        let cod = vec![1, 0, 0, 3, 1, 5, 3, 3, 0, 1, 0x44, 0x55, 0x66, 0x77, 0x77, 0x77];
        let mut coc = vec![1, 1]; coc.extend_from_slice(&cod[5..]);
        assert!(identical_coc(&coc, 3, &cod));
        coc[2] = 4; assert!(!identical_coc(&coc, 3, &cod));
        coc[2] = 5; coc[5] = 1; assert!(!identical_coc(&coc, 3, &cod));
        coc[5] = 0; coc[0] = 3; assert!(!identical_coc(&coc, 3, &cod));
    }
    #[test] fn local_plt_window_reuses_only_covered_header_bytes() {
        let window=Some((100,vec![1,2,3,4]));assert_eq!(header_window_prefix(101..103,&window),vec![2,3]);
        assert_eq!(header_window_prefix(102..110,&window),vec![3,4]);assert!(header_window_prefix(99..102,&window).is_empty());
        assert!(header_window_prefix(104..110,&window).is_empty());assert!(header_window_prefix(101..101,&window).is_empty());
    }
    #[test] fn adaptive_plt_lookahead_is_bounded_and_skips_nearby_or_small_headers() {
        let plan=|adaptive,latency,packets,remaining|plt_read_ahead_end(100,65637,2_000_000,1_048_676,packets,adaptive,latency,2,remaining);
        assert_eq!(plan(false,20000,243840,1_000_000),65637);
        assert_eq!(plan(true,1000,243840,1_000_000),65637);
        assert_eq!(plan(true,20000,1000,1_000_000),65637);
        assert_eq!(plan(true,20000,243840,1_000_000),487780);
        assert_eq!(plan(true,20000,243840,1000),65637);
        assert_eq!(plt_read_ahead_end(100,65637,100000,200000,243840,true,20000,2,1_000_000),100000);
    }
    #[test] fn header_cache_evicts_idle_payloads_but_preserves_active_leases() {
        let cache = HeaderCache { limit: 32, state: Mutex::new(HeaderCacheState::default()) };
        let first = Arc::new(HeaderPayload { main: vec![1; 32], container: vec![], tlm: vec![], jpip:vec![],jpip_locator:None });
        let weak = Arc::downgrade(&first);
        cache.put("one", first); let active = cache.get("one").unwrap();
        cache.put("two", Arc::new(HeaderPayload { main: vec![2; 32], container: vec![], tlm: vec![], jpip:vec![],jpip_locator:None }));
        assert!(cache.get("one").is_none());
        assert!(weak.upgrade().is_some());
        assert_eq!(active.main, vec![1; 32]);
        drop(active); assert!(weak.upgrade().is_none());
        assert_eq!(cache.state.lock().unwrap().bytes, 32);
    }
    #[test]
    fn header_working_set_preserves_weak_leases_within_the_byte_budget() {
        let cache=HeaderCache {limit:32*1024*1024,state:Mutex::new(HeaderCacheState::default())};
        let first=Arc::new(HeaderPayload {main:vec![1;60000],container:vec![],tlm:vec![],jpip:vec![],jpip_locator:None});
        let weak=Arc::downgrade(&first);cache.put("active-first",first); 
        for n in 0..512 {cache.put(&format!("working-{n}"),Arc::new(HeaderPayload {main:vec![2;60000],container:vec![],tlm:vec![],jpip:vec![],jpip_locator:None}));}
        assert!(weak.upgrade().is_some(),"count-only eviction forced a header reload despite available bytes");
        assert!(cache.get("active-first").is_some());
        for n in 512..600 {cache.put(&format!("working-{n}"),Arc::new(HeaderPayload {main:vec![3;60000],container:vec![],tlm:vec![],jpip:vec![],jpip_locator:None}));}
        let state=cache.state.lock().unwrap();assert!(state.bytes<=cache.limit);assert!(state.entries.len()<=MAX_HEADER_ENTRIES);drop(state);
        assert!(weak.upgrade().is_some(),"recent access must protect the active header during byte eviction");
    }
    #[test] fn header_digest_covers_container_and_tile_index_metadata() {
        let mut payload = HeaderPayload { main: vec![1], container: vec![2], tlm: vec![(Some(0), 14)], jpip:vec![],jpip_locator:None };
        let original = payload.digest(); payload.container[0] = 3;
        assert_ne!(original, payload.digest()); payload.container[0] = 2; payload.tlm[0].1 = 15;
        assert_ne!(original, payload.digest());
    }
    #[test] fn active_small_object_leases_survive_shared_cache_eviction_and_are_bounded() {
        let cache = RangeCache { limit: 1024, state: Mutex::new(CacheState::default()) };
        let identity = Arc::from("active-object");
        let leases = cache.put(&identity, 1024, 0, &[7; 4096]).into_iter().collect();
        let mut context = ReadContext::new(None);
        context.pin_small_object(leases);
        cache.put(&Arc::from("other-object"), 1024, 0, &[9; 1024]);
        assert!(cache.get(&identity, 1024, 0).is_none());
        assert_eq!(context.pinned.len(), 4);
        assert!(context.pinned.values().all(|block| block.iter().all(|byte| *byte == 7)));
        let mut oversized = HashMap::new();
        oversized.insert(0, Arc::from(vec![0; 8 * 1024 * 1024 + 1]));
        let mut unpinned = ReadContext::new(None);
        unpinned.pin_small_object(oversized);
        assert!(unpinned.pinned.is_empty());
    }
    #[test] fn final_tile_lengths_are_bounded_to_the_codestream_and_support_large_extents() {
        assert_eq!(resolved_part_length(0, 100, 116).unwrap(), 14);
        assert_eq!(resolved_part_length(0, 100, 8 * 1024 * 1024 * 1024 + 102).unwrap(), 8 * 1024 * 1024 * 1024);
        assert!(resolved_part_length(15, 100, 116).is_err());
        assert!(resolved_part_length(0, 100, 113).is_err());
        assert!(resolved_part_length(0, u64::MAX, 2).is_err());
    }
    #[test] fn range_merging_preserves_sparse_reads_and_respects_hard_limits() {
        let mut sparse = vec![0..1024, 500_000..501_024];
        merge_ranges(&mut sparse, 1, 2, 1024 * 1024, 2).unwrap();
        assert_eq!(sparse.len(), 2);
        assert!(merge_ranges(&mut sparse, 1, 1, 1024 * 1024, 2).is_err());
        let mut nearby = vec![0..1024, 2048..3072];
        merge_ranges(&mut nearby, 1, 1, 4096, 2).unwrap();
        assert_eq!(nearby, vec![0..3072]);
        let mut tight = vec![0..1024, 2048..3072];
        assert!(merge_ranges(&mut tight, 1, 1, 2048, 2).is_err());
    }
    #[test] fn index_retention_releases_capacity_not_just_elements() {
        let mut index = PartIndex { parts: Vec::with_capacity(4096), cursor: 114, complete: true };
        index.parts.push(TilePart { tile: 0, offset: 100, length: 14, ordinal: 0, total: 1 });
        let bytes = index.parts.capacity() * std::mem::size_of::<TilePart>();
        trim_retained_index(&mut index, 100, bytes);
        assert_eq!(index.parts.len(), 1);
        trim_retained_index(&mut index, 100, 0);
        assert_eq!(index.parts.capacity(), 0);
        assert_eq!(index.cursor, 100);
        assert!(!index.complete);
    }
    #[test] fn packet_lengths_reject_truncation_and_overflow() {
        assert_eq!(parse_plt(&[3, 0x81, 0]).unwrap(), vec![3, 128]);
        assert!(parse_plt(&[0x81]).is_err());
        assert!(parse_plt(&[0xff; 10]).is_err());
    }
    #[test] fn shared_cache_evicts_to_its_byte_budget() {
        let cache = RangeCache { limit: 4096, state: Mutex::new(CacheState::default()) };
        let one = Arc::from("one"); let two = Arc::from("two");
        cache.put(&one, BLOCK, 0, &[1; 4096]); cache.put(&two, BLOCK, 0, &[2; 4096]);
        assert!(cache.get(&one, BLOCK, 0).is_none()); assert_eq!(cache.get(&two, BLOCK, 0).unwrap()[0], 2);
        assert_eq!(cache.state.lock().unwrap().bytes, 4096);
    }
    #[test] fn cache_block_sizes_have_independent_alignment_and_shared_identity() {
        let cache = RangeCache { limit: 4096, state: Mutex::new(CacheState::default()) };
        let identity = Arc::from("same-version");
        cache.put(&identity, 1024, 0, &[1; 1024]);
        cache.put(&identity, 256, 0, &[2; 512]);
        assert_eq!(cache.get(&identity, 1024, 0).unwrap().len(), 1024);
        assert_eq!(cache.get(&identity, 256, 256).unwrap()[0], 2);
        let state = cache.state.lock().unwrap();
        assert!(state.blocks.keys().all(|(key, _, _)| Arc::ptr_eq(key, &identity)));
        assert_eq!(state.bytes, 1536);
    }
    #[test] fn request_drop_propagates_to_a_blocking_reader() {
        let (guard, receiver) = CancelOnDrop::new(); let context = ReadContext::new(Some(receiver));
        assert!(context.check().is_ok()); drop(guard); assert!(context.check().is_err());
    }
}

struct SourceProfile {
    started:Instant,
    stats:ReadStats,
    curl_requests:u64,
    // Caller-thread CPU distinguishes work from blocking. Cgroup CPU also
    // includes runtime workers; interpret it only on an isolated diagnostic.
    cpu:Option<(Option<u64>,Option<u64>)>,
}
fn source_cpu_counters()->(Option<u64>,Option<u64>) {
    let caller=std::fs::read_to_string("/proc/thread-self/schedstat").ok()
        .and_then(|s|s.split_whitespace().next()?.parse::<u64>().ok());
    let group=std::fs::read_to_string("/sys/fs/cgroup/cpu.stat").ok().and_then(|s|
        s.lines().find_map(|line|line.strip_prefix("usage_usec ").and_then(|v|v.parse::<u64>().ok()))
            .and_then(|v|v.checked_mul(1000)));
    (caller,group)
}
impl SourceProfile {
    fn new(context:&ReadContext)->Self {
        let cpu=std::env::var_os("RIVER_PROFILE_SOURCE").is_some().then(source_cpu_counters);
        Self {started:Instant::now(),stats:context.stats.clone(),curl_requests:context.curl_requests,cpu}
    }
}
fn profile_stage(stage: &str, context: &ReadContext, mark: &mut SourceProfile) {
    if let Some(before)=mark.cpu {
        let cpu=source_cpu_counters();
        let delta=|after:Option<u64>,before:Option<u64>|after.zip(before)
            .map(|(a,b)|a.saturating_sub(b) as f64/1_000_000.0).unwrap_or(-1.0);
        tracing::info!(stage,ms=mark.started.elapsed().as_secs_f64()*1000.0,
            caller_cpu_ms=delta(cpu.0,before.0),cgroup_cpu_ms=delta(cpu.1,before.1),
            requests=context.stats.requests-mark.stats.requests,bytes=context.stats.bytes-mark.stats.bytes,
            curl_requests=context.curl_requests-mark.curl_requests,
            "S3 preparation stage; cgroup CPU requires isolated requests; unavailable CPU=-1");
        mark.cpu=Some(source_cpu_counters());
    }
    mark.started=Instant::now();mark.stats=context.stats.clone();mark.curl_requests=context.curl_requests;
}

#[cfg(test)] mod single_tag_tree_tests {
 use super::*;
 #[test] fn inline_leaf_matches_general_tree_with_retries_and_stuffed_bytes() {
  for seed in 0..256u16 {
   let input=[seed as u8,255,127,0,255,0,85,170];
   let mut fast=TagTree::new(1,1);let mut reference=TagTree {nodes:vec![TagNode {value:u32::MAX,low:0,parent:None}],single:TagNode {value:u32::MAX,low:0,parent:None}};
   let mut a=Bits {bytes:&input,offset:0,remaining:0,byte:0,previous:0};let mut b=Bits {bytes:&input,offset:0,remaining:0,byte:0,previous:0};
   for threshold in [1,1,2,3,8,16,32,33] {
    assert_eq!(fast.decode(0,threshold,&mut a).unwrap(),reference.decode(0,threshold,&mut b).unwrap());assert_eq!((a.offset,a.remaining,a.byte,a.previous),(b.offset,b.remaining,b.byte,b.previous));
   }
  }
 }
}

#[cfg(test)]
mod ready_parts_tests;

fn known_header_probe(cached:bool,lowest:Option<u64>,inline:Option<u64>,part:&TilePart)->u64 {
 if cached {return 18;}
 if let Some(bytes)=lowest {return bytes;}
 if part.ordinal==0 && part.total<=1 {return inline.unwrap_or(18);}
 18
}

#[cfg(test)] mod known_header_probe_tests {
 use super::*;
 #[test] fn cached_headers_and_multipart_parts_keep_bounded_probes() {
  let mut part=TilePart{tile:0,offset:100,length:100000,ordinal:0,total:1};
  assert_eq!(known_header_probe(false,None,Some(8256),&part),8256);
  assert_eq!(known_header_probe(true,Some(2048),Some(8256),&part),18);
  assert_eq!(known_header_probe(false,None,None,&part),18);
  part.total=2;assert_eq!(known_header_probe(false,None,Some(8256),&part),18);
  assert_eq!(known_header_probe(false,Some(2048),Some(8256),&part),2048);
  part.total=0;part.ordinal=1;assert_eq!(known_header_probe(false,None,Some(8256),&part),18);
 }
}

fn discovery_prefix_fits(native:u64,probe:u64,block:u64,pinned:usize)->bool {
 probe>18 && native.saturating_mul(probe.saturating_add(block.saturating_mul(2)))
    .saturating_add(pinned as u64)<=8*1024*1024
}
#[cfg(test)]mod discovery_retention_tests {
 use super::*;
 #[test]fn recent_retention_reuses_arcs_and_never_fills_missing_blocks() {
  let mut context=ReadContext::new(None);let data:Arc<[u8]>=Arc::from(vec![7u8;1024]);
  context.recent.remember(1024,&[(0,data.clone()),(1024,data.clone())]);
  assert!(!context.pin_recent_range(1024,0..3072));assert!(context.pinned.is_empty());
  assert!(context.pin_recent_range(1024,0..2048));context.recent=recent_reads::RecentReads::default();
  assert!(Arc::ptr_eq(context.pinned.get(&0).unwrap(),&data));
  assert!(discovery_prefix_fits(451,8256,1024,0));
  assert!(!discovery_prefix_fits(451,32832,1024,0));
  assert!(!discovery_prefix_fits(451,8256,1024,8*1024*1024));
  let mut full=ReadContext::new(None);full.pinned.insert(0,Arc::from(vec![0u8;8*1024*1024]));
  full.recent.remember(1024,&[(8*1024*1024,data)]);assert!(!full.pin_recent_range(1024,8*1024*1024..8*1024*1024+1024));
 }
}
