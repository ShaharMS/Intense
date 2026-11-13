//! Stop index discovery once a resolution prefix is complete. Later parts are
//! neither opened nor downloaded; omitted packets become legal empty headers.
use super::*;
use std::sync::OnceLock;
type PrefixKey=(String,u8,Vec<u16>);
#[derive(Default)]
struct PrefixCache {lru:BTreeMap<u64,PrefixKey>,entries:HashMap<PrefixKey,(PreparedSubset,usize,u64)>,bytes:usize,clock:u64}
impl PrefixCache {
 fn trim(&mut self,limit:usize)->crate::resources::Occupancy {while self.bytes>limit||self.entries.len()>crate::resources::entry_limit("RIVER_S3_PREPARED_PREFIX_CACHE_ENTRIES",limit){let Some((_,key))=self.lru.pop_first()else{break;};if let Some(old)=self.entries.remove(&key){self.bytes-=old.1;crate::resources::evict("prepared_prefixes",1);}}crate::resources::Occupancy{bytes:self.bytes,entries:self.entries.len(),..Default::default()}}
 fn get(&mut self,key:&PrefixKey)->Option<PreparedSubset>{self.clock+=1;let now=self.clock;let Some(entry)=self.entries.get_mut(key)else{crate::resources::miss("prepared_prefixes");return None;};let old=entry.2;entry.2=now;let result=entry.0.clone();let bytes=entry.1;self.lru.remove(&old);self.lru.insert(now,key.clone());crate::resources::hit("prepared_prefixes",bytes);Some(result)}
 fn put(&mut self,key:PrefixKey,value:&PreparedSubset,limit:usize){if value.route!="s3-multipart-compact-lowest-prefix"||value.bytes.len()>4*1024*1024{return;}let limit=crate::resources::effective("prepared_prefixes",limit);let bytes=value.bytes.len()+key.0.len()+key.2.len()*2+256;if bytes>limit{return;}if let Some(old)=self.entries.remove(&key){self.bytes-=old.1;self.lru.remove(&old.2);}let _=self.trim(limit.saturating_sub(bytes));self.clock+=1;self.bytes+=bytes;self.lru.insert(self.clock,key.clone());self.entries.insert(key,(value.clone(),bytes,self.clock));}
}
fn prefix_cache()->&'static Mutex<PrefixCache>{static C:OnceLock<Mutex<PrefixCache>>=OnceLock::new();static REGISTER:std::sync::Once=std::sync::Once::new();let cache=C.get_or_init(||Mutex::new(PrefixCache::default()));REGISTER.call_once(||crate::resources::register("prepared_prefixes",move|limit|cache.lock().unwrap_or_else(|e|e.into_inner()).trim(limit)));cache}
fn prefix_cache_budget(value:Option<&str>)->usize{value.and_then(|v|v.parse().ok()).unwrap_or_else(||crate::resources::budget("prepared_prefixes"))}
fn prefix_cache_limit()->usize{prefix_cache_budget(None)}
struct Part {sot_offset:u64,header:Vec<u8>,range:Range<u64>,packets:usize,selected:usize}
struct Tile {entries:Vec<(u64,u32)>,wanted:usize,total:usize,covered:usize,parts:Vec<Part>,
 states:BTreeMap<(u8,u64,u64,u8),(layered_packets::State,usize)>}
fn cached_part_bytes(range:Range<u64>,block:u64,mut cached:impl FnMut(u64)->Option<Arc<[u8]>>)->Option<Vec<u8>> {
 let length=range.end.checked_sub(range.start)?;if length>16*1024 {return None;}if length==0 {return Some(vec![]);}
 let mut leases=vec![];
 for n in range.start/block..range.end.div_ceil(block) {
  let offset=n*block;let data=cached(offset)?;
  let a=range.start.saturating_sub(offset) as usize;let b=(range.end-offset).min(block) as usize;
  if b>data.len() || a>b {return None;}leases.push((data,a,b));
 }
 let mut bytes=Vec::with_capacity(length as usize);
 for (data,a,b) in leases {bytes.extend_from_slice(&data[a..b]);}Some(bytes)
}
pub(super) fn inline_candidate(h:&Header)->bool {
 h.packet_compatible && h.layers>1 && !h.is_ht && h.cod.first().is_some_and(|v|v&!1==0)
  && h.cod.get(8).is_some_and(|v|v&0xc0==0) && h.cod.get(9).is_some_and(|v|*v<=1) && matches!(h.progression,1|2)
}
// RLCP/RPCL place every layer of a resolution before the next resolution.
// Unlike inline packet-header probing, PLT lengths need no layer-state parser.
fn resolution_prefix(h:&Header,_layers:u32)->bool {
 h.packet_compatible && match h.progression {
  0=>h.layers==1 && h.prefix_compatible,
  // Retain all quality packets of the selected resolutions. The decoder
  // still applies the requested layer limit; no source layer is relabelled.
  1|2=>true,
  _=>false,
 }
}
pub(super) fn counts(h:&Header,tile:u16,wanted:u8)->Result<(usize,usize)> {
 let (x0,y0,x1,y1)=h.tile_bounds(tile);ensure!(x0<x1 && y0<y1,"invalid tile index");
 let mut prefix=0u64;let mut total=0u64;
 for r in 0..h.levels {
  let scale=1u32<<(h.levels-1-r);let pp=if h.cod[0]&1!=0 {h.cod[10+usize::from(r)]}else{0xff};
  let pw=1u32<<(pp&15);let ph=1u32<<(pp>>4);
  let n=u64::from(x1.div_ceil(scale).div_ceil(pw)-x0.div_ceil(scale)/pw)
   .checked_mul(u64::from(y1.div_ceil(scale).div_ceil(ph)-y0.div_ceil(scale)/ph))
   .and_then(|n|n.checked_mul(u64::from(h.components)))
   .and_then(|n|n.checked_mul(u64::from(h.layers))).context("packet count overflow")?;
  total=total.checked_add(n).context("packet count overflow")?;
  if r<=wanted {prefix=prefix.checked_add(n).context("packet count overflow")?;}
 }
 Ok((usize::try_from(prefix)?,usize::try_from(total)?))
}
// Read-ahead is only a bounded proposal. Every prefetched part still passes
// the ordinary SOT, marker and packet-count validation before it is used.
fn learned_header_probe(previous:Option<u64>,adaptive:bool,ratio:u64,latency_us:u64,block:u64)->u64 {
 if !adaptive || ratio<=1 || !(5000..u64::MAX).contains(&latency_us) {return 2048;}
 previous.unwrap_or(2048).max(2048).min(16*1024).div_ceil(block.max(1)).saturating_mul(block.max(1)).min(16*1024)
}
fn small_prefix_probe(entries:&[(u64,u32)],part:usize,parts:usize)->Range<u64> {
 small_prefix_probe_with_header(entries,part,parts,2048)
}
fn small_prefix_probe_with_header(entries:&[(u64,u32)],part:usize,parts:usize,header_probe:u64)->Range<u64> {
 let (offset,length)=entries[part];
 if length>16*1024 {return offset..offset+u64::from(length).min(header_probe);}
 let mut end=offset+u64::from(length);
 for &(next,length) in entries.iter().skip(part+1).take(parts.saturating_sub(1)) {
  if next!=end || length>16*1024 || next+u64::from(length)-offset>16*1024 {break;}
  end=next+u64::from(length);
 }
 offset..end
}
// TLM offsets are already validated, positive and non-overlapping. Equality
// between their total length and enclosing span proves a gap-free cluster.
// This is read-ahead only; it never establishes resolution/packet boundaries.
fn clustered_prefix_probe<'a>(entries:impl Iterator<Item=&'a Vec<(u64,u32)>>,parts:usize,levels:usize)->Option<Range<u64>> {
 if parts<2 {return None;}
 let (mut start,mut end,mut total,mut tiles)=(u64::MAX,0u64,0u64,0usize);
 for tile in entries {
  if tile.len()!=levels || tile.len()<parts {return None;}
  let mut bytes=0u64;
  for &(offset,length) in tile.iter().take(parts) {
   if !(14..=16*1024).contains(&length) {return None;}
   bytes=bytes.checked_add(u64::from(length))?;
   if bytes>16*1024 {return None;}
   start=start.min(offset);end=end.max(offset.checked_add(u64::from(length))?);
  }
  tiles+=1;total=total.checked_add(bytes)?;if total>8*1024*1024 {return None;}
 }
 (tiles>1 && end.checked_sub(start)==Some(total)).then_some(start..end)
}
impl RemoteSource {
 pub(super) fn prepare_discovered_multipart_prefix(&self,selected:&HashSet<u16>,payload:&HeaderPayload,parts:&[TilePart],reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let h=&self.header;
  if !payload.tlm.is_empty() || !payload.jpip.is_empty() || selected.len() as u64!=u64::from(h.layout.columns)*u64::from(h.layout.rows) {return Ok(None);}
  let mut cursor=h.first_part;let mut entries=Vec::with_capacity(parts.len());
  for part in parts {
   if part.offset!=cursor {return Ok(None);}
   cursor=cursor.checked_add(part.length).context("discovered prefix extent overflow")?;
   entries.push((Some(part.tile),u32::try_from(part.length)?));
  }
  if cursor!=h.codestream_end.saturating_sub(2) {return Ok(None);}
  let learned=HeaderPayload {main:payload.main.clone(),container:payload.container.clone(),tlm:entries,jpip:vec![],jpip_locator:None};
  self.prepare_multipart_prefix_enabled(selected,&learned,reduction,layers,context)
 }
 pub(super) fn prepare_unindexed_coarse_prefix(&self,selected:&HashSet<u16>,payload:&HeaderPayload,reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  if !std::env::var("RIVER_S3_UNINDEXED_COARSE_PREFIX").is_ok_and(|v|v=="true" || v=="1") {return Ok(None);}
  self.prepare_unindexed_coarse_prefix_enabled(selected,payload,reduction,layers,context)
 }
 pub(super) fn prepare_unindexed_coarse_prefix_enabled(&self,selected:&HashSet<u16>,payload:&HeaderPayload,reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let (limit,requests)=(context.limit,context.requests_limit);
  let explicit=context.explicit_request_limit;
  context.limit=limit.min(context.stats.bytes.saturating_add(8*1024*1024));
  context.requests_limit=requests.min(context.stats.requests.saturating_add(128));
  // Internal prefix allowances must not widen this speculative phase's cap.
  context.explicit_request_limit=true;
  let result=self.prepare_unindexed_coarse_prefix_inner(selected,payload,reduction,layers,context);
  context.limit=limit;context.requests_limit=requests;context.explicit_request_limit=explicit;
  match result {Err(error)=>{context.check()?;tracing::info!(context=context.trace_id,optimization="unindexed_coarse_prefix",reason=%error,action="alternate_read_path","Unindexed prefix candidate unavailable");Ok(None)},other=>other}
 }
 fn prepare_unindexed_coarse_prefix_inner(&self,selected:&HashSet<u16>,payload:&HeaderPayload,reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let h=&self.header;let native=u64::from(h.layout.columns)*u64::from(h.layout.rows);
  if !payload.tlm.is_empty() || !payload.jpip.is_empty() || h.layers<=1 || native<32 || selected.len() as u64!=native
   || reduction==0 || reduction>=h.levels || !resolution_prefix(h,layers) {return Ok(None);}
  let scale=1u32<<reduction;
  if u64::from(h.width.div_ceil(scale))*u64::from(h.height.div_ceil(scale))>512*512
   || h.cod[9]!=1 || h.image_origin!=(0,0) || h.layout.origin_x!=0 || h.layout.origin_y!=0
   || h.layout.width%scale!=0 || h.layout.height%scale!=0 {return Ok(None);}
  if resolution_view::project(&payload.main[2..],h,reduction,true)?.is_none() {return Ok(None);}
  // This bounds a candidate physical prefix, not the resolution represented
  // by an ordinal. Packet parsing below must prove the requested coverage.
  let depth=usize::from(h.levels-reduction);let target=selected.len().saturating_mul(depth);
  let metadata=target.saturating_mul(std::mem::size_of::<(Option<u16>,u32)>())
   .saturating_add(selected.len().saturating_mul(64))
   .saturating_add(payload.main.len()).saturating_add(payload.container.len());
  if !context.admit_metadata(metadata) {return Ok(None);}
  let result=(|| {
  let before=context.stats.clone();let mut cursor=h.first_part;let mut found=HashMap::<u16,usize>::with_capacity(selected.len());let mut index=Vec::with_capacity(target);
  while index.len()<target {
   context.check()?;
   if context.stats.requests.saturating_sub(before.requests)>=128 || context.stats.bytes.saturating_sub(before.bytes)>=8*1024*1024 {return Ok(None);}
   let cached=(cursor/self.block_size..cursor.saturating_add(12).div_ceil(self.block_size)).all(|block| {
    let offset=block*self.block_size;context.pinned.contains_key(&offset) || context.recent.contains(self.block_size,offset) || self.cache.get(&self.cache_key,self.block_size,offset).is_some()
   });
   if !cached {
    // After two validated first parts, estimate the remaining compact run.
    // This affects read-ahead only: every SOT and requested packet is still
    // checked below. A misleading layout can overfetch at most 256 KiB here.
    let lookahead=if index.len()>=2 {
     let observed=cursor.saturating_sub(h.first_part);
     (observed/(index.len() as u64)).saturating_mul((target-index.len()) as u64)
      .saturating_mul(5).div_ceil(4).clamp(16*1024,256*1024)
    }else{16*1024};
    let end=cursor.saturating_add(lookahead).min(h.codestream_end);
    if end-cursor>context.limit.saturating_sub(context.stats.bytes) {return Ok(None);}
    self.read(cursor..end,context)?;
   }
   let part=self.part_at(cursor,context)?;
   // This is only a candidate prefix. Do not infer packets or resolution from
   // ordinals; the existing packet parser below must prove requested coverage.
   if part.total==1 || part.length>16*1024 || !selected.contains(&part.tile) {return Ok(None);}
   let count=found.entry(part.tile).or_default();
   if *count>=depth || usize::from(part.ordinal)!=*count {return Ok(None);}*count+=1;
   index.push((Some(part.tile),u32::try_from(part.length)?));cursor=cursor.checked_add(part.length).context("unindexed prefix extent overflow")?;
  }
  let candidate=HeaderPayload {main:payload.main.clone(),container:payload.container.clone(),tlm:index,jpip:vec![],jpip_locator:None};
  match self.prepare_multipart_prefix_plan(selected,&candidate,reduction,layers,context,true) {
   Ok(result)=>Ok(result),
   Err(error)=>{context.check()?;tracing::info!(context=context.trace_id,optimization="unindexed_coarse_prefix",reason=%error,action="alternate_read_path","First-part run did not cover requested packets");Ok(None)}
  }
  })();
  context.metadata_bytes=context.metadata_bytes.saturating_sub(metadata);
  result
 }
 pub(super) fn prepare_multipart_prefix(&self,selected:&HashSet<u16>,payload:&HeaderPayload,reduction:u8,layers:u32,
    context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  if !std::env::var("RIVER_S3_MULTIPART_PREFIX").is_ok_and(|v|v=="true" || v=="1") {return Ok(None);}
  let result=self.prepare_multipart_prefix_enabled(selected,payload,reduction,layers,context);
  if payload.tlm.is_empty() && !payload.jpip.is_empty() {
   match result {
    Err(error)=>{context.check()?;tracing::info!(context=context.trace_id,optimization="resolution_prefix",index_source="jpip_tpix",reason=%error,action="alternate_read_path","Optional indexed resolution prefix unavailable");Ok(None)}
    other=>other,
   }
  }else{result}
 }
 pub(super) fn prepare_multipart_prefix_enabled(&self,selected:&HashSet<u16>,payload:&HeaderPayload,reduction:u8,layers:u32,
    context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  self.prepare_multipart_prefix_plan(selected,payload,reduction,layers,context,false)
 }
 fn prepare_multipart_prefix_plan(&self,selected:&HashSet<u16>,payload:&HeaderPayload,reduction:u8,layers:u32,
    context:&mut ReadContext,first_parts_only:bool)->Result<Option<PreparedSubset>> {
  let h=&self.header;
  let profile=std::env::var_os("RIVER_PROFILE_SOURCE").is_some();
  let (mut order_ns,mut read_ns,mut parse_ns,mut attempts,mut empty_headers,mut buffered_parts)=(0u128,0u128,0u128,0u64,0u64,0u64);
  if !resolution_prefix(h,layers) || reduction==0 {return Ok(None);}
  // A complete TPix table supplies the same ordered tile/length information
  // as TLM. The metadata parser proved contiguous codestream coverage; every
  // part actually used below still validates its SOT tile, extent and ordinal.
  let derived=payload.tlm.is_empty().then(||payload.jpip.iter().map(|e|(Some(e.tile),e.length)).collect::<Vec<_>>());
  let tile_index=derived.as_deref().unwrap_or(&payload.tlm);
  if tile_index.is_empty() || tile_index.iter().any(|(tile,_)|tile.is_none()) {return Ok(None);}
  // On small objects, retaining every low-resolution quality layer can add
  // many header GETs compared with the established whole-tile path. Keep
  // that path for partial-quality requests unless explicitly size-disabled.
  if h.layers>1 && layers>0 && layers<u32::from(h.layers)
   && self.meta.size<=env_limit("RIVER_S3_SMALL_OBJECT_BYTES",8*1024*1024) as u64 {return Ok(None);}
  let mut cache_tiles=selected.iter().copied().collect::<Vec<_>>();cache_tiles.sort_unstable();
  let cache_key=(self.id.clone(),reduction,cache_tiles);
  if prefix_cache_limit()>0 {
   if let Some(prepared)=prefix_cache().lock().unwrap_or_else(|e|e.into_inner()).get(&cache_key) {
    context.check()?;
    tracing::debug!(context=context.trace_id,bytes=prepared.bytes.len(),"Reusing complete-quality compact prefix");
    return Ok(Some(prepared));
   }
  }
  let scale=1u32<<reduction;
  let eligible=h.layers>1 && !h.is_ht && h.cod[9]==1 && h.cod[8]&0xc0==0 && h.cod[0]&6==0
   && h.image_origin==(0,0) && h.layout.origin_x==0 && h.layout.origin_y==0
   && h.layout.width%scale==0 && h.layout.height%scale==0;
  let projection_started=profile.then(Instant::now);
  let main_projection=if eligible {resolution_view::project(&payload.main[2..],h,reduction,true)?}else{None};
  let projection_ns=projection_started.map_or(0,|t|t.elapsed().as_nanos());
  let fill=1+usize::from(h.cod[0]&2!=0)*6+usize::from(h.cod[0]&4!=0)*2;
  // New classic styles must not turn the established small-object path into
  // hundreds of dependent packet-header reads. Preserve indexed prefixes;
  // unindexed small complete views can fetch the object in bounded chunks.
  let prefer_small_object=h.cod[8]!=0 && self.meta.size<=env_limit("RIVER_S3_SMALL_OBJECT_BYTES",8*1024*1024) as u64
   && selected.len() as u64==u64::from(h.layout.columns)*u64::from(h.layout.rows);
  if prefer_small_object {
   let first=self.part_at(h.first_part,context)?;
   let (_,start,lengths,compatible)=self.tile_header(&first,context)?;
   let usable=compatible && lengths.as_ref().is_some_and(|v|v.iter().all(|&n|n>0)
    && v.iter().try_fold(start,|at,&n|at.checked_add(n))==Some(first.offset+first.length));
   if !usable {return Ok(None);}
  }
  let offsets=tlm_offsets(h.first_part,h.codestream_end,tile_index)?;
  let mut entries=BTreeMap::<u16,Vec<(u64,u32)>>::new();
  for ((tile,length),offset) in tile_index.iter().zip(offsets) {
   let tile=tile.unwrap();if selected.contains(&tile) {entries.entry(tile).or_default().push((offset,*length));}
  }
  if !first_parts_only && !entries.values().any(|parts|parts.len()>1) {return Ok(None);}
  ensure!(entries.len()==selected.len(),"tile-part index missing requested JPEG2000 tile");
  // No more offsets will be appended. Avoid charging geometric Vec growth
  // across every selected tile when the exact TLM chain is already known.
  for parts in entries.values_mut() {parts.shrink_to_fit();}
  let metadata=entries.values().map(|parts|parts.capacity().saturating_mul(std::mem::size_of::<(u64,u32)>())).sum::<usize>()
   .saturating_add(selected.len().saturating_mul(std::mem::size_of::<Tile>()+std::mem::size_of::<Part>()+64));
  if !context.admit_metadata(metadata) {return Ok(None);}
  let wanted=h.levels-1-reduction;let mut tiles=BTreeMap::new();
  for (tile,entries) in entries {
   ensure!(entries.len()<=256,"multipart prefix tile exceeds ordinal capacity");
   let (wanted,total)=counts(h,tile,wanted)?;
   // Compact views omit high-resolution packet placeholders entirely. Keep
   // the legacy bound only when main-header projection cannot be used.
   if main_projection.is_none() && total.checked_mul(fill).is_none_or(|bytes|bytes>context.limit as usize) {return Ok(None);}
   tiles.insert(tile,Tile {entries,wanted,total,covered:0,parts:Vec::with_capacity(1),states:BTreeMap::new()});
  }
  // Same bounded default prefix allowance as ordinary tiled discovery. Explicit
  // total caps still win; the per-source simultaneous-read cap is unchanged.
  context.allow_prefix_ranges(selected.len() as u64,context.requests_limit,0);
  if h.layers>1 && selected.len() as u64==u64::from(h.layout.columns)*u64::from(h.layout.rows)
   && self.meta.size>env_limit("RIVER_S3_SMALL_OBJECT_BYTES",8*1024*1024) as u64 {
   if let Some(range)=clustered_prefix_probe(tiles.values().map(|t|&t.entries),usize::from(wanted+1).min(3),usize::from(h.levels)) {
    let proposal=vec![(true,range.clone())];
    if range.end-range.start+self.block_size*2<=context.limit.saturating_sub(context.stats.bytes)
     && context.can_pin_ranges(self.block_size,&proposal) {
     let leases=self.prefetch_ranges(&proposal,context,ADAPTIVE_PREFIX_RANGES)?;context.pin_packet_blocks(leases);
    }
   }
  }
  loop {
   context.check()?;
   let mut probes=vec![];let mut ahead=vec![];
   for tile in tiles.values().filter(|t|t.covered<t.wanted) {
    let &(offset,length)=tile.entries.get(tile.parts.len()).context("multipart prefix does not cover requested resolution")?;
    // A small part's complete extent is bounded by TLM. Fuse its header and
    // entropy to avoid a dependent RTT; larger parts retain a tiny header probe.
    let cached_header=length>16*1024 && (offset/self.block_size..(offset+16).div_ceil(self.block_size)).all(|block| {
     let key=block*self.block_size;
     context.pinned.contains_key(&key) || context.recent.contains(self.block_size,key) || self.cache.get(&self.cache_key,self.block_size,key).is_some()
    });
    let count=if length<=16*1024 {u64::from(length)}else if cached_header {16}else{2048.min(u64::from(length))};
    probes.push((true,offset..offset+count));
    let lookahead=if h.layers>1 && tile.parts.is_empty() && tile.entries.len()==usize::from(h.levels)
     && self.meta.size>env_limit("RIVER_S3_SMALL_OBJECT_BYTES",8*1024*1024) as u64 {
      usize::from(wanted+1).min(3)
     }else{1};
    let settings=crate::read_policy::settings();
    let previous=tile.parts.last().map(|part|part.range.start.saturating_sub(part.sot_offset));
    let header_probe=learned_header_probe(previous,settings.mode==crate::read_policy::Mode::Adaptive && h.layers>1,
     u64::from(settings.max_overfetch_ratio).min(crate::read_policy::explicit_overfetch_cap().unwrap_or(16)),self.planning_latency_us(),self.block_size);
    ahead.push((true,if cached_header {offset..offset+16}else{small_prefix_probe_with_header(&tile.entries,tile.parts.len(),lookahead,header_probe)}));
   }
   if probes.is_empty() {break;}
   let ahead_bytes:u64=ahead.iter().map(|(_,r)|r.end-r.start).sum();
   let rounded_bound=ahead_bytes.saturating_add(ahead.len() as u64*self.block_size*2);
   if ahead_bytes<=8*1024*1024 && rounded_bound<=context.limit.saturating_sub(context.stats.bytes)
    && context.can_pin_ranges(self.block_size,&ahead) {probes=ahead;}
   let leases=self.prefetch_ranges(&probes,context,ADAPTIVE_PREFIX_RANGES)?;context.pin_packet_blocks(leases);
   for (&tile_id,tile) in tiles.iter_mut().filter(|(_,t)|t.covered<t.wanted) {
    let (offset,length)=tile.entries[tile.parts.len()];let sot=self.part_at(offset,context)?;
    ensure!(sot.tile==tile_id && sot.length==u64::from(length),"tile-part index does not match SOT");
    ensure!(usize::from(sot.ordinal)==tile.parts.len(),"multipart prefix ordinals are missing or duplicated");
    ensure!(first_parts_only || sot.total==0 || usize::from(sot.total)==tile.entries.len(),"multipart prefix total disagrees with tile-part index");
    let (header,start,lengths,compatible)=self.tile_header(&sot,context)?;
    // SOT identity is validated above and the plan retains its exact offset.
    // A second request-cache copy is not needed by subsequent prefix phases.
    if context.validated.remove(&sot.offset).is_some() {
     context.metadata_bytes=context.metadata_bytes.saturating_sub(std::mem::size_of::<TilePart>()+64);
    }
    // This plan owns the header now. Do not retain a second request-local
    // header cache across thousands of tiles and exhaust the metadata budget.
    if let Some((_,parsed))=context.tile_headers.remove(&sot.offset) {
     let bytes=parsed.0.capacity()+parsed.2.as_ref().map_or(0,|v|v.capacity()*8)+128+std::mem::size_of::<TilePart>();
     context.metadata_bytes=context.metadata_bytes.saturating_sub(bytes);
    }
    if !compatible {return Ok(None);}
    if !context.admit_metadata(header.capacity()) {return Ok(None);}
    // Some encoders put a tile-wide PLT in the first tile-part. It cannot
    // address this part's bytes. Use independently parsed inline headers,
    // rather than trusting lengths that run into the next SOT/header.
    let lengths=lengths.filter(|lengths|lengths.iter().all(|&n|n>0)
     && lengths.iter().try_fold(start,|at,&n|at.checked_add(n))==Some(sot.offset+sot.length));
    let inline=lengths.is_none();
    let lengths=if let Some(lengths)=lengths {lengths} else {
     // With inline headers, carry tag-tree and codeblock state through every
     // quality layer. TLM locates parts; parsed packets prove the exact prefix.
     // Do not infer resolution boundaries from tile-part ordinals or lengths.
     if prefer_small_object || !inline_candidate(h) {return Ok(None);}
     let temporary=tile.wanted.saturating_mul(std::mem::size_of::<Packet>()).saturating_add(96);
     if !context.admit_metadata(temporary) {return Ok(None);}
     let began=profile.then(Instant::now);
     let (packets,_)=packet_order_layers(h,tile_id,Some(wanted),true)?;
     if let Some(t)=began {order_ns+=t.elapsed().as_nanos();}
     // Parse an already-pinned small part from one buffer. Repeated64-byte
     // probes otherwise allocate/copy/hash for every quality packet. Never
     // fetch additional source bytes merely to enable this CPU optimization.
     let part_end=sot.offset+sot.length;
     // Every wanted inline packet consumes at least one byte. Before this
     // validated part ends, min(remaining packet count,payload bytes) is a
     // required contiguous prefix, not guessed resolution/quality data.
     let minimum=(tile.wanted-tile.covered) as u64;
     let minimum=minimum.min(part_end-start);
     if minimum>16*1024 {
      let required=vec![(true,start..start+minimum)];
      if context.can_pin_ranges(self.block_size,&required) {
       let leases=self.prefetch_ranges(&required,context,usize::MAX)?;context.pin_packet_blocks(leases);
      }
     }
     let began=profile.then(Instant::now);
     let buffered=if part_end-start<=context.limit {
      cached_part_bytes(start..part_end,self.block_size,|offset|
       context.pinned.get(&offset).cloned().or_else(||context.recent.get(self.block_size,offset))
        .or_else(||self.cache.get(&self.cache_key,self.block_size,offset)))
     }else{None};
     if let Some(bytes)=&buffered {context.stats.cache_bytes+=bytes.len() as u64;buffered_parts+=1;}
     if let Some(t)=began {read_ns+=t.elapsed().as_nanos();}
     let mut lengths=vec![];let mut cursor=start;
     for (packet_index,packet) in packets.iter().skip(tile.covered).take(tile.wanted-tile.covered).enumerate() {
      context.check()?;
      if cursor==sot.offset+sot.length {break;}
      ensure!(cursor<sot.offset+sot.length,"multilayer packet exceeds tile-part payload");
      let key=(packet.resolution,packet.x,packet.y,packet.component);
      if !tile.states.contains_key(&key) {
       let Some(grids)=prefix_codeblocks(h,tile_id,packet)? else{return Ok(None);};
       let size=grids.iter().map(|&(w,h)|w*h*192+256).sum::<usize>();
       if !context.admit_metadata(size) {return Ok(None);}
       tile.states.insert(key,(layered_packets::State::new_lazy(&grids,h.cod[8]),size));
      }
      let state=&mut tile.states.get_mut(&key).unwrap().0;let mut probe=64u64;
      let (header,body)=loop {
       let owned;let end;
       let bytes=if let Some(data)=&buffered {
        end=part_end;&data[usize::try_from(cursor-start)?..]
       }else{
        end=cursor.saturating_add(probe).min(part_end);
        let remaining=tile.wanted-tile.covered-packet_index;
        let minimum=(remaining as u64).min(part_end-cursor);
        if probe==64 && minimum>=self.block_size {
         let cached=(cursor/self.block_size..end.div_ceil(self.block_size)).all(|block| {
          let key=block*self.block_size;
          context.pinned.contains_key(&key) || context.recent.contains(self.block_size,key) || self.cache.get(&self.cache_key,self.block_size,key).is_some()
         });
         if !cached {
          let required=vec![(true,cursor..cursor+minimum)];
          if context.can_pin_ranges(self.block_size,&required) {
           let leases=self.prefetch_ranges(&required,context,usize::MAX)?;context.pin_packet_blocks(leases);
          }
         }
        }
        let began=profile.then(Instant::now);
        owned=self.read(cursor..end,context)?;if let Some(t)=began {read_ns+=t.elapsed().as_nanos();}&owned[..]
       };
       let began=profile.then(Instant::now);let parsed=if buffered.is_some() {state.length_from_complete_part(bytes,packet.layer)}else{state.length(bytes,packet.layer)};
       if let Some(t)=began {parse_ns+=t.elapsed().as_nanos();attempts+=1;if parsed.is_ok() && bytes.first().is_some_and(|b|b&0x80==0) {empty_headers+=1;}}
       match parsed {
        Ok(result)=>break result,
        Err(error) if error.downcast_ref::<ProbeTooShort>().is_some()=>{
         ensure!(end<sot.offset+sot.length && probe<MAX_HEADER as u64,"truncated multilayer packet header");
         probe=(probe*2).min(MAX_HEADER as u64);
        },
        Err(error)=>return Err(error),
       }
      };
      let length=(header as u64).checked_add(body).context("multilayer packet length overflow")?;
      cursor=cursor.checked_add(length).context("multilayer packet extent overflow")?;
      ensure!(cursor<=sot.offset+sot.length,"multilayer packet exceeds tile-part payload");
      lengths.push(length);
      if packet.layer+1==h.layers {
       let (_,bytes)=tile.states.remove(&key).unwrap();
       context.metadata_bytes=context.metadata_bytes.saturating_sub(bytes);
      }
     }
     context.metadata_bytes=context.metadata_bytes.saturating_sub(temporary);
     Arc::new(lengths)
    };
    let end_packet=tile.covered.checked_add(lengths.len()).context("multipart prefix packet count overflow")?;
    ensure!(end_packet<=tile.total,"multipart prefix packet count exceeds codestream geometry");
    let packet_count=lengths.len();let selected=packet_count.min(tile.wanted-tile.covered);
    let mut selected_end=start;let mut cursor=start;
    for (index,length) in lengths.iter().copied().enumerate() {
     ensure!(length>0,"invalid zero-length multipart prefix packet");
     let end=cursor.checked_add(length).context("multipart prefix packet length overflow")?;
     ensure!(end<=sot.offset+sot.length,"multipart prefix packet exceeds tile part");
     if index<selected {selected_end=end;}cursor=end;
    }
    // Inline parsing may stop inside the final selected part once every
    // requested resolution/layer is proven. PLT still validates full coverage.
    ensure!(cursor==sot.offset+sot.length || (inline && end_packet==tile.wanted),
     "multipart prefix packet lengths do not cover tile-part payload");
    if end_packet>=tile.wanted {
     let released=tile.entries.capacity().saturating_mul(std::mem::size_of::<(u64,u32)>());
     tile.entries=Vec::new();context.metadata_bytes=context.metadata_bytes.saturating_sub(released);
    }
    if tile.parts.len()==tile.parts.capacity() {
     let additional=tile.parts.capacity().max(1);
     if !context.admit_metadata(additional.saturating_mul(std::mem::size_of::<Part>())) {return Ok(None);}
     tile.parts.reserve_exact(additional);
    }
    tile.covered=end_packet;tile.parts.push(Part {sot_offset:sot.offset,header,range:start..selected_end,packets:packet_count,selected});
   }
  }
  // Fetch the still-missing prefix packets across tiles together. Most small
  // resolution parts were already acquired in the fused first-header wave.
  let required=tiles.values().flat_map(|t|t.parts.iter().filter(|p|!p.range.is_empty()).map(|p|(true,p.range.clone()))).collect::<Vec<_>>();
  let leases=self.prefetch_ranges(&required,context,usize::MAX)?;context.pin_packet_blocks(leases);
  let build_began=profile.then(Instant::now);
  // Low-resolution reversible views need only their original
  // resolution-prefix packets. Preserve every quality layer, project geometry
  // and quantization, and omit the millions of synthetic high-band packets.
  // Header validation is transactional: unsupported overrides retain the old view.
  // Classical codeblock styles affect coding passes, not resolution geometry.
  // Preserve those bits and the original entropy. The inline parser already
  // handles BYPASS/TERMALL segment lengths; HT and reserved styles stay out.
  let complete_grid=selected.len() as u64==u64::from(h.layout.columns)*u64::from(h.layout.rows);
  let mut projected=main_projection;
  if projected.is_some() {
   for part in tiles.values().flat_map(|tile|&tile.parts) {
    context.check()?;
    if resolution_view::project(&part.header,h,reduction,false)?.is_none() {projected=None;break;}
   }
  }
  let compact=projected.is_some();
  // A tile-header override may prevent projection even after main-header
  // eligibility. Recheck before allocating any legacy high-band placeholders.
  if !compact && tiles.values().any(|tile|tile.total.checked_mul(fill).is_none_or(|bytes|bytes>context.limit as usize)) {return Ok(None);}
  let mut stream=if let Some(mut main)=projected {main.splice(..0,[0xff,0x4f]);main}else{payload.main.clone()};
  for tile in tiles.values() {
   let count=tile.parts.len();
   for (ordinal,part) in tile.parts.iter().enumerate() {
    let mut bytes=self.selected_payload(&[(true,part.range.clone())],context)?;
    let omitted=if compact {0}else{part.packets-part.selected+if ordinal+1==count {tile.total-tile.covered}else{0}};
    for index in part.selected..part.selected+omitted {append_empty_packet(&mut bytes,h.cod[0],index);}
    let mut sot=self.read(part.sot_offset..part.sot_offset+12,context)?;
    sot[11]=u8::try_from(count).unwrap_or(0);
    let projected_header=if compact {Some(resolution_view::project(&part.header,h,reduction,false)?.context("validated compact tile header changed")?)}else{None};
    let header=projected_header.as_deref().unwrap_or(&part.header);
    sot[6..10].copy_from_slice(&u32::try_from(12+header.len()+2+bytes.len())?.to_be_bytes());
    stream.extend(sot);stream.extend_from_slice(header);stream.extend([0xff,0x93]);stream.extend(bytes);
    ensure!(stream.len()<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"compressed subset exceeds request memory budget");
   }
  }
  stream.extend([0xff,0xd9]);let jp2=!payload.container.is_empty();
  let bytes=if jp2 {let mut bytes=payload.container.clone();if compact {resize_jp2(&mut bytes,h.width.div_ceil(scale),h.height.div_ceil(scale))?;}bytes.extend(u32::try_from(stream.len()+8)?.to_be_bytes());bytes.extend(b"jp2c");bytes.extend(stream);bytes}else{stream};
  if let Some(t)=build_began {tracing::info!(target:"river_server::remote_source::multipart_profile",packet_order_ms=order_ns as f64/1e6,cached_probe_read_ms=read_ns as f64/1e6,packet_parse_ms=parse_ns as f64/1e6,stream_build_ms=(t.elapsed().as_nanos()+projection_ns) as f64/1e6,parse_attempts=attempts,empty_headers,buffered_parts,tiles=tiles.len(),"multilayer prefix CPU profile");}
  // Sparse projected views keep the ordinary ROI decoder. The parallel
  // renderer requires a complete native grid and must not claim their workers.
  let route=if !compact {"s3-multipart-prefix-ranges"}else if complete_grid {"s3-multipart-compact-lowest-prefix"}else{"s3-multipart-partial-compact-prefix"};
  let prepared=PreparedSubset {view_reduction:if compact {reduction}else{0},technical_origin:None,bytes,jp2,route,width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0};
  prefix_cache().lock().unwrap_or_else(|e|e.into_inner()).put(cache_key,&prepared,prefix_cache_limit());
  Ok(Some(prepared))
 }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test]fn seekable_openjpeg_handles_short_reads_and_reports_failed_io() {
  use std::io::{Read,Seek,SeekFrom};
  struct Reader {data:std::io::Cursor<&'static [u8]>,reads:usize,bytes:usize,fail_after:usize}
  impl Read for Reader {
   fn read(&mut self,out:&mut [u8])->std::io::Result<usize> {
    if self.reads>=self.fail_after{return Err(std::io::Error::other("injected source read failure"));}
    self.reads+=1;let n=out.len().min(997);let n=self.data.read(&mut out[..n])?;self.bytes+=n;Ok(n)
   }
  }
  impl Seek for Reader {fn seek(&mut self,at:SeekFrom)->std::io::Result<u64>{self.data.seek(at)}}
  for bytes in [include_bytes!("../../tests/fixtures/indexed-cidx/unindexed-resolution-major.jp2").as_slice(),include_bytes!("../../tests/fixtures/indexed-cidx/unindexed-tile-major.jp2").as_slice()] {
   let mut reader=Reader {data:std::io::Cursor::new(bytes),reads:0,bytes:0,fail_after:usize::MAX};
   let image=jpeg2k::Image::from_reader_with(&mut reader,bytes.len() as u64,jpeg2k::DecodeParameters::new().reduce(3).threads(1)).unwrap();
   let gold=include_bytes!("../../tests/fixtures/indexed-cidx/r3-all.gray");let planes=image.components();
   assert_eq!((planes[0].width(),planes[0].height()),(64,64));assert!(planes[0].data().iter().zip(gold).all(|(&a,&b)|a==i32::from(b)));
   assert!(reader.reads>1);
   let mut reader=Reader {data:std::io::Cursor::new(bytes),reads:0,bytes:0,fail_after:2};
   assert!(jpeg2k::Image::from_reader_with(&mut reader,bytes.len() as u64,jpeg2k::DecodeParameters::new().reduce(3).threads(1)).is_err());
  }
 }
 #[tokio::test]async fn unindexed_first_parts_require_real_packet_coverage() {
  use std::io::Write;
  let runtime=tokio::runtime::Handle::current();
  tokio::task::spawn_blocking(move||{
   let inputs:[(&[u8],bool);3]=[
    (include_bytes!("../../tests/fixtures/indexed-cidx/unindexed-resolution-major.jp2"),true),
    (include_bytes!("../../tests/fixtures/indexed-cidx/unindexed-tile-major.jp2"),false),
    (include_bytes!("../../tests/fixtures/indexed-cidx/unindexed-quality-split.jp2"),false),
   ];
   for (case,(bytes,eligible)) in inputs.into_iter().enumerate() {
    let mut file=tempfile::NamedTempFile::new().unwrap();file.write_all(bytes).unwrap();let padding=9*1024*1024u32;
    file.write_all(&padding.to_be_bytes()).unwrap();file.write_all(b"free").unwrap();file.as_file().set_len(bytes.len() as u64+u64::from(padding)).unwrap();file.flush().unwrap();
    let path=file.path().to_owned();let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
    let location=Path::from(path.file_name().unwrap().to_str().unwrap());let meta=runtime.block_on(store.head(&location)).unwrap();let id=format!("unindexed-prefix-{}",uuid::Uuid::new_v4());
    let (source,_)=RemoteSource::open(id.clone(),id,store,location,meta,runtime.clone(),Arc::new(RangeCache {limit:16*1024*1024,state:Mutex::new(CacheState::default())})).unwrap();
    let mut context=ReadContext::new(None);let payload=source.acquire_render_header(&mut context).unwrap();assert!(payload.tlm.is_empty() && payload.jpip.is_empty());
    for layers in [1,0] {
     let before=context.stats.clone();let original_limits=(context.limit,context.requests_limit,context.explicit_request_limit);
     let result=source.prepare_unindexed_coarse_prefix_enabled(&(0..64).collect(),&payload,3,layers,&mut context).unwrap();
     assert_eq!((context.limit,context.requests_limit,context.explicit_request_limit),original_limits,"temporary candidate limits must be restored");
     if !eligible {assert!(result.is_none());assert!(context.stats.requests-before.requests<=if case==1 {2}else{128},"unsupported layout must obey candidate read cap");continue;}
     let prepared=result.unwrap();assert_eq!(prepared.view_reduction,3);
     let image=jpeg2k::Image::from_bytes_with(&prepared.bytes,jpeg2k::DecodeParameters::new().threads(1).layers(layers)).unwrap();let planes=image.components();
     let gold:&[u8]=if layers==1 {include_bytes!("../../tests/fixtures/indexed-cidx/r3-l1.gray")}else{include_bytes!("../../tests/fixtures/indexed-cidx/r3-all.gray")};
     assert_eq!((planes[0].width(),planes[0].height()),(64,64));assert!(planes[0].data().iter().zip(gold).all(|(&a,&b)|a==i32::from(b)));
    }
    let limits=(context.limit,context.requests_limit,context.explicit_request_limit);
    let next=source.prepare_unindexed_coarse_prefix_enabled(&(0..64).collect(),&payload,2,0,&mut context).unwrap();
    assert_eq!((context.limit,context.requests_limit,context.explicit_request_limit),limits);
    if eligible {
     let prepared=next.unwrap();assert_eq!(prepared.view_reduction,2);
     let image=jpeg2k::Image::from_bytes_with(&prepared.bytes,jpeg2k::DecodeParameters::new().threads(1)).unwrap();let planes=image.components();
     assert_eq!((planes[0].width(),planes[0].height()),(128,128));
     assert!(planes[0].data().iter().zip(include_bytes!("../../tests/fixtures/indexed-cidx/r2-all.gray")).all(|(&a,&b)|a==i32::from(b)));
    }else{assert!(next.is_none(),"Ordering or incomplete quality must not establish next-level coverage");}
    let selected=(0..64).collect();
    let parts=source.selected_parts(&selected,&payload,16384,14,false,true,&mut context).unwrap();
    let mut broken=parts.clone();broken[0].offset+=1;
    assert!(source.prepare_discovered_multipart_prefix(&selected,&payload,&broken,3,0,&mut context).unwrap().is_none());
    assert!(source.prepare_discovered_multipart_prefix(&selected,&payload,&parts[..parts.len()-1],3,0,&mut context).unwrap().is_none());
    let prepared=source.prepare_discovered_multipart_prefix(&selected,&payload,&parts,3,0,&mut context).unwrap().unwrap();
    let image=jpeg2k::Image::from_bytes_with(&prepared.bytes,jpeg2k::DecodeParameters::new().threads(1)).unwrap();let planes=image.components();
    assert_eq!((planes[0].width(),planes[0].height()),(64,64));
    assert!(planes[0].data().iter().zip(include_bytes!("../../tests/fixtures/indexed-cidx/r3-all.gray")).all(|(&a,&b)|a==i32::from(b)));
   }
  }).await.unwrap();
 }
 #[test] fn prefix_cache_configuration_accepts_zero_and_caps_budget() {
  assert_eq!(prefix_cache_budget(Some("0")),0);
  assert_eq!(prefix_cache_budget(None),crate::resources::budget("prepared_prefixes"));
  assert_eq!(prefix_cache_budget(Some("invalid")),crate::resources::budget("prepared_prefixes"));
  assert_eq!(prefix_cache_budget(Some("1048576")),1048576);
  assert_eq!(prefix_cache_budget(Some("999999999")),999999999);
 }
 #[test] fn complete_quality_prefix_cache_bounds_versions_geometry_and_owned_bytes() {
  let prepared=PreparedSubset {view_reduction:3,technical_origin:None,bytes:vec![7;100],route:"s3-multipart-compact-lowest-prefix",jp2:false,width:128,height:128,layout:NativeTileLayoutHint {width:128,height:128,columns:1,rows:1,origin_x:0,origin_y:0},offset_x:0,offset_y:0};
  let key=("version-a".into(),3,vec![0]);let mut cache=PrefixCache::default();
  cache.put(key.clone(),&prepared,500);assert!(cache.bytes<=500);
  let mut copy=cache.get(&key).unwrap();copy.bytes[0]=99;assert_eq!(cache.get(&key).unwrap().bytes[0],7);
  assert!(cache.get(&("version-b".into(),3,vec![0])).is_none());
  assert!(cache.get(&("version-a".into(),2,vec![0])).is_none());
  assert!(cache.get(&("version-a".into(),3,vec![1])).is_none());
  let other=("other".into(),3,vec![0]);cache.put(other.clone(),&prepared,500);
  assert!(cache.get(&key).is_none());assert!(cache.get(&other).is_some());assert!(cache.bytes<=500);
  let mut partial=prepared.clone();partial.route="s3-multipart-partial-compact-prefix";
  cache.put(key.clone(),&partial,500);assert!(cache.get(&key).is_none());
  cache.put(key.clone(),&prepared,0);assert!(cache.get(&key).is_none());
 }
 #[test] fn cached_part_copy_keeps_exact_offsets_tail_and_missing_block_fallback() {
  let original=(0..2500).map(|i|(i%251) as u8).collect::<Vec<_>>();
  let cache=original.chunks(1024).enumerate().map(|(i,b)|(i as u64*1024,Arc::<[u8]>::from(b))).collect::<HashMap<_,_>>();
  assert_eq!(cached_part_bytes(100..2500,1024,|n|cache.get(&n).cloned()).unwrap(),original[100..2500]);
  assert!(cached_part_bytes(100..2500,1024,|n|if n==1024 {None}else{cache.get(&n).cloned()}).is_none());
  assert!(cached_part_bytes(100..2501,1024,|n|cache.get(&n).cloned()).is_none());
  let mut calls=0;assert!(cached_part_bytes(0..16385,1024,|_|{calls+=1;None}).is_none());assert_eq!(calls,0);
  assert_eq!(cached_part_bytes(100..100,1024,|_|None),Some(vec![]));
 }
 #[test] fn clustered_first_parts_require_a_complete_bounded_physical_run() {
  let a=vec![(100,20),(140,30),(200,500)];let b=vec![(120,20),(170,30),(700,500)];
  assert_eq!(clustered_prefix_probe([&a,&b].into_iter(),2,3),Some(100..200));
  assert_eq!(clustered_prefix_probe([&a,&b].into_iter(),1,3),None);
  let gap=vec![(120,20),(180,30),(700,500)];assert_eq!(clustered_prefix_probe([&a,&gap].into_iter(),2,3),None);
  let oversized=vec![(0,9000),(9000,9000),(18000,14)];assert_eq!(clustered_prefix_probe([&oversized,&b].into_iter(),2,3),None);
  assert_eq!(clustered_prefix_probe([&a].into_iter(),2,3),None);
 }
 #[test] fn small_read_ahead_is_bounded_contiguous_and_never_infers_packet_boundaries() {
  let entries=[(100,200),(300,500),(800,700),(1500,20000)];assert_eq!(small_prefix_probe(&entries,0,3),100..1500);assert_eq!(small_prefix_probe(&entries,0,1),100..300);assert_eq!(small_prefix_probe(&entries,2,3),800..1500);assert_eq!(small_prefix_probe(&entries,3,3),1500..3548);
  assert_eq!(small_prefix_probe(&[(0,100),(200,100)],0,3),0..100);assert_eq!(small_prefix_probe(&[(0,8000),(8000,8000),(16000,1000)],0,3),0..16000);
 }
 #[test] fn lazy_counts_match_materialized_orders_in_shifted_clipped_grids() {
  for progression in 0..5 {for image_origin in [(0,0),(13,29)] {
   let h=Header {image_origin,width:1703,height:1301,components:3,display_rgb_only:false,bit_depth:8,levels:6,layers:1,
    layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:2,rows:2},
    progression,is_ht:false,cod:vec![1,progression,0,1,0,5,3,3,0,1,0x22,0x33,0x44,0x55,0x66,0x77],
    first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:true,payload_digest:[0;32]};
   for tile in 0..4 {for wanted in 0..h.levels {
    let (prefix,total)=counts(&h,tile,wanted).unwrap();let (packets,expected)=packet_order(&h,tile,Some(wanted)).unwrap();
    assert_eq!((prefix,total),(packets.len(),expected));
   }}
  }}
 }
 #[test] fn multilayer_resolution_prefix_retains_all_layers_for_decoder_layer_limits() {
  let mut h=Header {image_origin:(0,0),width:128,height:128,components:3,display_rgb_only:false,bit_depth:8,levels:7,layers:10,
   layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:128,height:128,columns:1,rows:1},
   progression:2,is_ht:false,cod:vec![0,2,0,10,1,6,4,4,0,1],first_part:0,codestream_end:100,
   packet_compatible:true,prefix_compatible:false,payload_digest:[0;32]};
  assert_eq!(counts(&h,0,0).unwrap(),(30,210));
  assert!(resolution_prefix(&h,0));assert!(resolution_prefix(&h,10));
  assert!(resolution_prefix(&h,1));assert!(resolution_prefix(&h,5));
  h.progression=1;assert!(resolution_prefix(&h,0));
  for progression in [0,3,4] {h.progression=progression;assert!(!resolution_prefix(&h,0));}
  h.progression=2;h.packet_compatible=false;assert!(!resolution_prefix(&h,0));
 }
}

#[cfg(test)] mod learned_header_tests {
 use super::*;
 #[test] fn validated_header_size_guides_only_latency_sensitive_adaptive_reads() {
  assert_eq!(learned_header_probe(Some(7699),true,2,16000,1024),8192);
  assert_eq!(learned_header_probe(None,true,2,16000,1024),2048);
  assert_eq!(learned_header_probe(Some(1048576),true,2,16000,1024),16384);
  for (adaptive,ratio,latency) in [(false,2,16000),(true,1,16000),(true,2,4000),(true,2,u64::MAX)] {assert_eq!(learned_header_probe(Some(7699),adaptive,ratio,latency,1024),2048);}
 }
 #[test] fn learned_probe_is_clipped_to_current_part_and_preserves_small_part_fusion() {
  assert_eq!(small_prefix_probe_with_header(&[(100,20000)],0,1,8192),100..8292);
  let small=[(100,2000),(2100,2000),(4100,20000)];assert_eq!(small_prefix_probe_with_header(&small,0,3,8192),small_prefix_probe(&small,0,3));
 }
}
