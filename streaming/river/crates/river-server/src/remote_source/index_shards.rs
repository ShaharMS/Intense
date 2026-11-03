//! Speculative parallel SOT chains. Seeds are never trusted offsets: only
//! a chain connected exactly to the main header's Psot walk may be committed.
//! This permits even false SOT signatures in COM/PLT payloads without accepting
//! their metadata or changing a rendered pixel. No tile ordering is assumed.
use super::*;
use tracing::Instrument;
use std::sync::atomic::AtomicBool;

#[derive(Debug)]
pub(super) struct Lane {pub parts:Vec<TilePart>,pub cursor:u64}
#[derive(Debug,Default)]
pub(super) struct State {pub attempted:bool,pub lanes:Vec<Lane>}
impl RemoteSource {
    pub(super) fn index_lanes(&self,index:&PartIndex,selected:&HashSet<u16>,whole_view:bool,attempted:&mut bool,context:&mut ReadContext)->Result<Option<Vec<Lane>>> {
        let native=u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows);
        let latency=self.minimum_range_latency_us();
        // Many small native tiles on nearby storage benefit from more independent
        // chains. Keep the same16MiB probe ceiling and bandwidth/contention guards.
        let average=self.meta.size/native.max(1);
        // Narrow seeds benefit a constrained frontier. With more frontier
        // lanes, wider seeds avoid long dependent gaps between large parts.
        // Both layouts retain the same probe-byte ceiling. Explicit overrides
        // and partial views keep their existing behavior.
        let frontier=env_limit("RIVER_S3_RANGE_CONCURRENCY",16).clamp(1,16)
            .min(crate::read_policy::settings().effective_source_requests());
        let automatic=if native>1024 && whole_view && latency<2000 {(2*16*1024*1024/average.max(64*1024)).clamp(4,32) as usize}
            else if whole_view && latency>=2000 && frontier<=4 {16} else {4};
        let explicit_count=std::env::var("RIVER_S3_INDEX_SHARDS").ok().and_then(|v|v.parse::<usize>().ok());
        let mut count=explicit_count.unwrap_or(automatic).min(32);
        if count==0 || self.meta.size<64*1024*1024 || native<128 || latency<minimum_latency(native) || latency==u64::MAX
            || index.complete || selected.iter().copied().max().unwrap_or(0)<64 {return Ok(None);}
        // Preserve speculative latency hiding when the source has spare lanes.
        // Concurrent required walks must not pay extra bulk traffic once they
        // already fill source admission. Keep the nearby large-grid guard too.
        let active=crate::source_admission::active_preparations();
        let source_preparations=self.read_profile.active_preparations();
        let limit=crate::read_policy::settings().effective_source_requests();
        let active_reads=self.read_profile.active_operations();
        if active_reads>=limit || source_preparations>=limit || (active>1 && native>1024 && latency<2000) {
            *attempted=true;
            tracing::info!(context=context.trace_id,source_preparations,active_source_reads=active_reads,source_limit=limit,action="required_reads_only","Speculative index probes skipped while required reads use source capacity");
            return Ok(Some(vec![]));
        }
        if native>1024 && latency<2000 {
            // Required reads also give concurrently arriving jobs time to enter.
            if index.parts.len()<8 {return Ok(None);}
        }
        let remaining=context.limit.saturating_sub(context.stats.bytes);
        // At most a quarter of the normal compressed-read budget, reserving
        // the rest for required headers/payload. Peak probe leases <=16 MiB.
        let budget=std::env::var("RIVER_S3_INDEX_PROBE_BYTES").ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(16*1024*1024).min(16*1024*1024).min(remaining/4);
        let pilot_bytes=64*1024;
        if explicit_count.is_none() {
            let observed=index.parts.iter().rev().map(|part|part.length).filter(|length|*length>=64*1024).take(16).collect::<Vec<_>>();
            count=learned_shard_count(count,whole_view,latency,frontier,budget.saturating_sub(pilot_bytes),
                &observed,crate::read_policy::settings().mode==crate::read_policy::Mode::Adaptive);
        }
        let window=(budget.saturating_sub(pilot_bytes)/count as u64).min(4*1024*1024)/self.block_size*self.block_size;
        if window<64*1024 || context.requests_limit.saturating_sub(context.stats.requests)<(count*4+32) as u64 {return Ok(None);}
        let end=self.header.codestream_end.saturating_sub(2);
        // Spatial order is only a placement hint. Failed or unordered hints
        // cannot eliminate any sequential work or establish tile completeness.
        let fraction=if whole_view {native}else{u64::from(selected.iter().copied().max().unwrap_or(0))+1};
        let image_area=u128::from(self.header.width)*u128::from(self.header.height);
        let covered=if whole_view {image_area}else{covered_area(self.header.width,self.header.height,self.header.image_origin,self.header.tile_bounds(*selected.iter().max().unwrap()))};
        let hinted=self.header.first_part+(u128::from(end-self.header.first_part)*covered/image_area.max(1)) as u64;
        if hinted<=index.cursor.saturating_add(window*2) {return Ok(None);}
        let mut spans=vec![];
        for n in 1..=count {
            let center=index.cursor+(hinted-index.cursor)*n as u64/(count as u64+1);
            let start=center.saturating_sub(window/2).max(index.cursor).min(end.saturating_sub(window))/self.block_size*self.block_size;
            spans.push((true,start..(start+window).min(end)));
        }
        *attempted=true; // Cancellation must not restart a fresh probe budget on every pan.
        // A cheap timed pilot prevents 16 MiB of speculative traffic from
        // making slow links much worse. Cached hits do not estimate bandwidth.
        let pilot=spans[0].1.start..spans[0].1.start+pilot_bytes;
        let before=context.stats.bytes;let started=Instant::now();let deadline=context.deadline;
        context.deadline=deadline.min(started+Duration::from_millis(500));
        let measured=self.prefetch_ranges(&[(true,pilot)],context,usize::MAX);
        context.deadline=deadline;
        let leases=match measured {
            Ok(leases)=>leases,
            Err(error)=>{
                context.check()?;
                if error.chain().any(|cause|cause.to_string().contains("S3 range timed out")) {return Ok(Some(vec![]));}
                return Err(error);
            }
        };
        let elapsed=started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        context.pin_packet_blocks(leases);
        let expected_parts=if whole_view {native}else{fraction}.saturating_sub(index.parts.len() as u64);
        let body_us=elapsed.saturating_sub(latency).max(if native>1024 {100}else{1000});
        let planned=window*count as u64;
        let transfer_us=u128::from(body_us)*u128::from(planned)/u128::from(pilot_bytes);
        let savings_us=u128::from(expected_parts)*u128::from(latency)/2;
        if context.stats.bytes>before && transfer_us>savings_us {
            tracing::debug!(elapsed_us=elapsed,network_us=latency,"SOT probe skipped: bandwidth cost exceeds likely RTT saving");
            return Ok(Some(vec![]));
        }
        // Full-image previews need every tile's prefix: keep their established
        // full windows. Partial views can save speculative entropy traffic.
        let staged=(selected.len() as u64)<native;
        let initial=spans.iter().map(|(_,range)| {
            let length=if staged {((range.end-range.start)*3/4)/self.block_size*self.block_size} else {range.end-range.start};
            (true,range.start..range.start+length)
        }).collect::<Vec<_>>();
        let leases=self.prefetch_ranges(&initial,context,usize::MAX)?;
        let mut lanes=vec![];let mut rescue=vec![];let mut tails=HashMap::new();
        for ((_,range),(_,full)) in initial.into_iter().zip(spans) {
            let data=probe_bytes(&leases,self.block_size,&range);
            if let Some(lane)=seed(&data,range.start,index.cursor,native,end) {
                if !lanes.iter().any(|old:&Lane|old.parts[0].offset==lane.parts[0].offset) {lanes.push(lane);}
            } else if range.end<full.end {
                let tail=(range.end-1)/self.block_size*self.block_size;
                if let Some(block)=leases.get(&tail) {tails.insert(tail,block.clone());}
                rescue.push((true,range.end.saturating_sub(12)..full.end));
            }
        }
        // Keeping at most one old block per window avoids re-reading a SOT
        // which crosses the prefix boundary, including with the shared cache off.
        context.pin_packet_blocks(tails);
        if !rescue.is_empty() {
            let leases=self.prefetch_ranges(&rescue,context,usize::MAX)?;
            for (_,range) in rescue {
                let data=probe_bytes(&leases,self.block_size,&range);
                if let Some(lane)=seed(&data,range.start,index.cursor,native,end) {
                    if !lanes.iter().any(|old:&Lane|old.parts[0].offset==lane.parts[0].offset) {lanes.push(lane);}
                }
            }
        }
        tracing::debug!(lanes=lanes.len(),probe_bytes=budget,"SOT parallel index seeds; not yet connected to source chain");
        Ok(Some(lanes))
    }

    pub(super) fn deep_index_depth(&self,whole:bool,context:&ReadContext)->usize {
        if (self.cache.limit as u64)<context.limit || crate::source_admission::active_preparations()>1 {return 1;}
        let native=u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows);
        let automatic=if native>1024 && self.minimum_range_latency_us()<2000 {if whole {32}else{128}}else{1};
        let depth=env_limit("RIVER_S3_INDEX_LANE_DEPTH",automatic).clamp(1,128);
        let partial=!whole && !std::env::var("RIVER_S3_INDEX_PARTIAL_LANES").is_ok_and(|v|v=="false" || v=="0");
        if whole || partial {depth}else{1}
    }

    pub(super) fn advance_index_lanes(&self,lanes:&mut Vec<Lane>,cursor:u64,probe:u64,selected:&HashSet<u16>,whole:bool,context:&mut ReadContext)->Result<()> {
        let depth=self.deep_index_depth(whole,context);
        if depth>1 {return self.advance_deep(lanes,cursor,probe,depth,(!whole).then(||selected.clone()),context);}
        let end=self.header.codestream_end.saturating_sub(2);
        let native=u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows);
        let capacity=if native>1024 {4096}else{128};
        let mut spans=vec![(true,cursor..cursor.saturating_add(probe.max(12)).min(self.header.codestream_end))];
        for lane in lanes.iter().filter(|lane|lane.cursor<end && lane.parts.len()<capacity) {spans.push((true,lane.cursor..lane.cursor.saturating_add(probe.max(12)).min(self.header.codestream_end)));}
        let leases=self.prefetch_ranges(&spans,context,usize::MAX)?;context.pin_packet_blocks(leases);
        // Only bounded 12-byte SOT parsing follows the batched IO. A bogus
        // seed dies locally; cancellation/version/budget errors still escape.
        let native=u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows);
        for lane in lanes.iter_mut().filter(|lane|lane.cursor<end && lane.parts.len()<capacity) {
            context.check()?;
            loop {
                match self.part_at(lane.cursor,context) {
                    Ok(part) if u64::from(part.tile)<native && (part.total==0 || part.ordinal<part.total)=>{lane.cursor+=part.length;lane.parts.push(part);},
                    _=>{context.check()?;lane.cursor=self.header.codestream_end;break;}
                }
                if lane.cursor>=end || lane.parts.len()>=capacity {break;}
                // Empty/short parts and seed windows often contain several
                // further SOTs. Drain those now instead of wasting another
                // network wave merely to advance an already cached header.
                let cached=context.validated.contains_key(&lane.cursor) || (lane.cursor/self.block_size..lane.cursor.saturating_add(12).div_ceil(self.block_size)).all(|block| {
                    let offset=block*self.block_size;
                    context.pinned.contains_key(&offset) || context.recent.contains(self.block_size,offset) || self.cache.get(&self.cache_key,self.block_size,offset).is_some()
                });
                if !cached {break;}
                context.check()?;
            }
        }
        Ok(())
    }
}

// Large grids can benefit from parallelism even below a millisecond.
pub(super) fn minimum_latency(native:u64)->u64 {if native>1024 {0}else{2000}}
#[derive(Debug,PartialEq)]
pub(super) enum Calibration {Fast,Waiting,Ready}
pub(super) fn calibration(native:u64,required_reads:u64,minimum_us:u64)->Calibration {
    if minimum_us<minimum_latency(native) {Calibration::Fast}
    else if required_reads<INDEX_CALIBRATION_READS {Calibration::Waiting}
    else {Calibration::Ready}
}
pub(super) fn trim_retained(lanes:&mut Vec<Lane>) {
    for lane in lanes {
        let live=lane.parts.last().is_some_and(|p|p.offset+p.length==lane.cursor);
        if lane.parts.len()>128 {
            lane.parts.truncate(128);
            if live {lane.cursor=lane.parts.last().map(|p|p.offset+p.length).unwrap_or(lane.cursor);}
        }
        if lane.parts.capacity()>128 {lane.parts.shrink_to_fit();}
    }
}

fn covered_area(width:u32,height:u32,origin:(u32,u32),bounds:(u32,u32,u32,u32))->u128 {
    let (_,y0,x1,y1)=bounds;
    (u128::from(y0.saturating_sub(origin.1))*u128::from(width)
        +u128::from(y1.saturating_sub(y0))*u128::from(x1.saturating_sub(origin.0)))
        .min(u128::from(width)*u128::from(height))
}
fn probe_bytes(leases:&HashMap<u64,Arc<[u8]>>,block_size:u64,range:&Range<u64>)->Vec<u8> {
    let mut data=Vec::with_capacity((range.end-range.start) as usize);let mut cursor=range.start;
    while cursor<range.end {
        let block=cursor/block_size*block_size;
        let Some(bytes)=leases.get(&block) else {break;};let within=(cursor-block) as usize;
        let take=(range.end-cursor).min((bytes.len()-within) as u64) as usize;
        data.extend_from_slice(&bytes[within..within+take]);cursor+=take as u64;
    }
    data
}
fn seed(data:&[u8],start:u64,after:u64,native:u64,end:u64)->Option<Lane> {
    for (i,bytes) in data.windows(12).enumerate() {
        if bytes[..4]!=[0xff,0x90,0,10] {continue;}
        let offset=start+i as u64;let length=u64::from(u32::from_be_bytes(bytes[6..10].try_into().unwrap()));
        let tile=u16::from_be_bytes(bytes[4..6].try_into().unwrap());
        if offset<=after || length<14 || offset.saturating_add(length)>end || u64::from(tile)>=native || (bytes[11]>0 && bytes[10]>=bytes[11]) {continue;}
        return Some(Lane {cursor:offset+length,parts:vec![TilePart {tile,offset,length,ordinal:bytes[10],total:bytes[11]}]});
    }
    None
}

/// Connecting by exact Psot offsets, rather than tile IDs or byte proximity,
/// is the correctness gate. Unreachable branches can never become an index.
pub(super) fn connected(lanes:&mut Vec<Lane>,cursor:u64)->Option<Vec<TilePart>> {
    let lane=lanes.iter().enumerate().filter_map(|(n,lane)|lane.parts.iter().position(|p|p.offset==cursor).map(|i|(n,i,lane.parts.len()-i))).max_by_key(|v|v.2)?;
    let mut parts=lanes.remove(lane.0).parts;parts.drain(..lane.1);Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_delay_cannot_trigger_bulk_probes_before_fast_required_reads_arrive() {
        let mut minimum=u64::MAX;let mut decisions=vec![];
        for (n,latency) in [6000,5000,4000,300,400,350,500,300].into_iter().enumerate() {
            minimum=minimum.min(latency);decisions.push(calibration(451,n as u64+1,minimum));
        }
        assert_eq!(&decisions[..3],&[Calibration::Waiting,Calibration::Waiting,Calibration::Waiting]);
        assert!(decisions[3..].iter().all(|v|*v==Calibration::Fast));
    }
    #[test]
    fn sustained_latency_and_large_grids_still_become_eligible_after_required_reads() {
        for native in [451,4096] {
            for required in 0..8 {assert_eq!(calibration(native,required,3000),Calibration::Waiting);}
            assert_eq!(calibration(native,8,3000),Calibration::Ready);
        }
        assert_eq!(calibration(451,2,300),Calibration::Fast);
    }
    fn part(offset:u64,length:u64)->TilePart {TilePart {tile:0,offset,length,ordinal:0,total:1}}
    #[test]
    fn retained_lanes_resume_after_the_bounded_prefix() {
        let parts=(0..1024).map(|n|part(100+n*14,14)).collect();
        let mut lanes=vec![Lane {parts,cursor:100+1024*14}];trim_retained(&mut lanes);
        assert_eq!(lanes[0].parts.len(),128);assert!(lanes[0].parts.capacity()<=128);assert_eq!(lanes[0].parts[0].offset,100);assert_eq!(lanes[0].cursor,100+128*14);
        let mut dead=vec![Lane {parts:(0..1024).map(|n|part(100+n*14,14)).collect(),cursor:999999}];
        trim_retained(&mut dead);assert_eq!(dead[0].cursor,999999);assert!(dead[0].parts.capacity()<=128);
    }
    #[test]
    fn clipped_row_and_column_use_area_and_respect_reference_origins() {
        assert_eq!(covered_area(1300,700,(0,0),(0,512,512,700)),512*1300+188*512);
        assert_eq!(covered_area(1300,700,(32,64),(32,576,544,764)),512*1300+188*512);
        assert_eq!(covered_area(1300,700,(0,0),(1024,512,1300,700)),1300*700);
    }
    #[test]
    fn rescue_can_recover_a_seed_crossing_the_prefix_boundary() {
        let mut data=vec![0;40];data[15..27].copy_from_slice(&[255,144,0,10,0,2,0,0,0,100,0,1]);
        assert!(seed(&data[..24],1000,0,8,2000).is_none());
        let lane=seed(&data[12..],1012,0,8,2000).unwrap();assert_eq!(lane.parts[0].offset,1015);assert_eq!(lane.cursor,1115);
    }
    #[test]
    fn false_embedded_markers_cannot_be_committed() {
        let mut lanes=vec![Lane {parts:vec![part(111,20),part(131,20)],cursor:151}];
        // Real chain goes from 100 straight to 200, jumping over the fake COM data.
        assert!(connected(&mut lanes,100).is_none());assert!(connected(&mut lanes,200).is_none());
    }
    #[test]
    fn merges_only_the_connected_suffix_and_preserves_its_offsets() {
        let mut lanes=vec![Lane {parts:vec![part(100,20),part(120,25),part(145,30)],cursor:175}];
        let merged=connected(&mut lanes,120).unwrap();assert_eq!(merged,vec![part(120,25),part(145,30)]);
    }
}

// The real cursor can continue through selected single-part tiles while
// other requested IDs remain. Speculative roots still pause at the first one.
fn stop_requested(part:&TilePart,requested:Option<&HashSet<u16>>,primary:bool,found:&mut HashSet<u16>)->bool {
    let Some(requested)=requested else{return false;};
    if !requested.contains(&part.tile){return false;}
    if !primary || part.ordinal!=0 || part.total!=1 {return true;}
    found.insert(part.tile);found.len()==requested.len()
}

#[cfg(test)] mod selected_stop_tests {
 use super::*;
 fn tile(id:u16)->TilePart {TilePart {tile:id,offset:100,length:14,ordinal:0,total:1}}
 #[test] fn primary_stops_at_the_last_pending_id_without_counting_duplicates(){
  let requested=HashSet::from([2,4]);let mut found=HashSet::new();
  assert!(!stop_requested(&tile(2),Some(&requested),true,&mut found));
  assert!(!stop_requested(&tile(2),Some(&requested),true,&mut found));
  assert!(!stop_requested(&tile(3),Some(&requested),true,&mut found));
  assert!(stop_requested(&tile(4),Some(&requested),true,&mut found));
 }
 #[test] fn speculative_and_fragmented_parts_keep_the_earlier_stop(){
  let requested=HashSet::from([2,4]);let mut found=HashSet::new();
  assert!(stop_requested(&tile(2),Some(&requested),false,&mut found));
  let mut fragmented=tile(2);fragmented.total=2;
  assert!(stop_requested(&fragmented,Some(&requested),true,&mut found));
  assert!(found.is_empty());
 }
 #[test] fn single_target_and_whole_walk_stop_behavior_is_preserved(){
  let requested=HashSet::from([2]);let mut found=HashSet::new();
  assert!(stop_requested(&tile(2),Some(&requested),true,&mut found));
  assert!(!stop_requested(&tile(2),None,true,&mut found));
 }
}

impl RemoteSource {
 fn advance_deep(&self,lanes:&mut Vec<Lane>,cursor:u64,probe:u64,depth:usize,stop_tiles:Option<HashSet<u16>>,context:&mut ReadContext)->Result<()> {
  context.check()?;let _fetch=self.fetch_admission(context)?;
  let cod_end=self.header.codestream_end;let end=cod_end.saturating_sub(2);let native=u64::from(self.header.layout.columns)*u64::from(self.header.layout.rows);
  let stop_tiles=stop_tiles.map(Arc::new);
  let batch_selected=!std::env::var("RIVER_S3_INDEX_BATCH_SELECTED").is_ok_and(|v|v=="false" || v=="0");
  let capacity=if native>1024 {4096}else{128};let mut roots=lanes.iter().filter_map(|l|l.parts.first().map(|p|p.offset)).collect::<Vec<_>>();roots.sort_unstable();roots.dedup();
  let stop=|root:u64|roots.iter().copied().find(|p|*p>root).unwrap_or(end);
  let latency=self.minimum_range_latency_us().max(50);let depth=depth.min((8000/latency).max(1) as usize).min((8*1024*1024/((lanes.len()+1) as u64*(probe+2*self.block_size))).max(1) as usize);
  let mut jobs=vec![(None,cursor,stop(cursor),depth)];
  for (n,lane) in lanes.iter().enumerate() {
   if lane.cursor>=end || lane.parts.len()>=capacity || contains_requested(lane,stop_tiles.as_deref()) {continue;}
   let root=lane.parts.first().map(|p|p.offset).unwrap_or(lane.cursor);let limit=stop(root);
   if lane.cursor<limit {jobs.push((Some(n),lane.cursor,limit,depth.min(32).min(capacity-lane.parts.len())));}
  }
  let stopped=Arc::new(AtomicBool::new(false));let budget=Arc::new(Mutex::new(ReadStats::default()));let before=context.stats.clone();let bytes_limit=context.limit;let request_limit=context.requests_limit;
  let client=self.range_client(context);let cache=self.cache.clone();let key=self.cache_key.clone();let block=self.block_size;let size=self.meta.size;let deadline=context.deadline;let cancel=context.cancel.clone();
  let concurrency=env_limit("RIVER_S3_RANGE_CONCURRENCY",16).clamp(1,16);
  let result=self.runtime.block_on(async {
   let mut pending=jobs.into_iter();let mut tasks=tokio::task::JoinSet::new();
   let launch=|tasks:&mut tokio::task::JoinSet<Result<(Option<usize>,Lane)>>,(id,start,limit,steps):(Option<usize>,u64,u64,usize)| {
    let stop_tiles=stop_tiles.clone();let stopped=stopped.clone();let client=client.clone();let cache=cache.clone();let key=key.clone();let budget=budget.clone();let cancel=cancel.clone();let before=before.clone();
    tasks.spawn(async move {
     let mut local=HashMap::new();let mut at=start;let mut parts=vec![];let mut dead=false;let mut found=HashSet::new();
     for _ in 0..steps {
      ensure!(!stopped.load(Ordering::Relaxed),"SOT lane batch abandoned");
      if at>=limit || at>=end || crate::source_admission::active_preparations()>1 {break;}
      if cancel.as_ref().is_some_and(|c|*c.borrow()) {bail!("S3 request superseded");}
      ensure!(Instant::now()<deadline,"S3 range timed out");
      if cached_header(&cache,&key,block,at,&mut local).is_none() {
       let last=at.saturating_add(probe.max(12)).min(limit.max(at+12)).min(cod_end);
       let mut spans:Vec<Range<u64>>=vec![];
       for n in at/block..last.div_ceil(block) {
        let offset=n*block;if local.contains_key(&offset) {continue;}
        if let Some(data)=cache.get(&key,block,offset) {local.insert(offset,data);continue;}
        if let Some(r)=spans.last_mut().filter(|r|r.end==offset) {r.end=(offset+block).min(size);} else {spans.push(offset..(offset+block).min(size));}
       }
       for range in spans {
        ensure!(!stopped.load(Ordering::Relaxed),"SOT lane batch abandoned");
        {let mut b=budget.lock().unwrap_or_else(|e|e.into_inner());ensure!(before.bytes+b.bytes+range.end-range.start<=bytes_limit,"S3 compressed-read budget exceeded");ensure!(before.requests+b.requests<request_limit,"S3 range request limit exceeded");b.bytes+=range.end-range.start;b.requests+=1;}
        let data=client.fetch(range.clone(),deadline,cancel.clone()).await?;local.extend(cache.put(&key,block,range.start,&data));
       }
      }
      let Some(bytes)=cached_header(&cache,&key,block,at,&mut local) else {bail!("missing fetched SOT header");};
      let part=parse_header(&bytes,at,native,cod_end);
      let Some(part)=part else {dead=true;break;};let stop=stop_requested(&part,stop_tiles.as_deref(),id.is_none() && batch_selected,&mut found);at+=part.length;parts.push(part);if stop {break;}
     }
     Ok((id,Lane {parts,cursor:if dead {cod_end}else{at}}))
    }.in_current_span());
   };
   for job in pending.by_ref().take(concurrency) {launch(&mut tasks,job);}
   while let Some(out)=tasks.join_next().await {
    let completed=out.context("SOT lane worker failed").and_then(|v|v).and_then(|v|context.check().map(|_|v));
    let (id,lane)=match completed {Ok(value)=>value,Err(error)=>{
     stopped.store(true,Ordering::Relaxed);tasks.abort_all();while tasks.join_next().await.is_some() {}return Err(error);
    }};
    if let Some(id)=id {lanes[id].cursor=lane.cursor;lanes[id].parts.extend(lane.parts);} else if !lane.parts.is_empty() {lanes.push(lane);}
    if let Some(job)=pending.next() {launch(&mut tasks,job);}
   }
   Ok::<_,anyhow::Error>(())
  });
  let used=budget.lock().unwrap_or_else(|e|e.into_inner());context.stats.bytes+=used.bytes;context.stats.requests+=used.requests;result
 }
}
fn cached_header(cache:&RangeCache,key:&Arc<str>,block:u64,at:u64,local:&mut HashMap<u64,Arc<[u8]>>)->Option<[u8;12]> {
 let mut out=[0;12];let mut done=0;
 while done<12 {
  let position=at+done as u64;let offset=position/block*block;
  if !local.contains_key(&offset) {local.insert(offset,cache.get(key,block,offset)?);}
  let data=local.get(&offset)?;let start=(position-offset) as usize;let count=(12-done).min(data.len().checked_sub(start)?);if count==0 {return None;}
  out[done..done+count].copy_from_slice(&data[start..start+count]);done+=count;
 }Some(out)
}
fn parse_header(b:&[u8;12],at:u64,native:u64,end:u64)->Option<TilePart> {
 if b[..4]!=[255,144,0,10] {return None;}let tile=u16::from_be_bytes([b[4],b[5]]);if u64::from(tile)>=native || (b[11]>0 && b[10]>=b[11]) {return None;}
 let declared=u32::from_be_bytes(b[6..10].try_into().ok()?);let length=resolved_part_length(declared,at,end).ok()?;Some(TilePart {tile,offset:at,length,ordinal:b[10],total:b[11]})
}

pub(super) fn prune_covered(lanes:&mut Vec<Lane>,cursor:u64) {
 lanes.retain(|lane|lane.parts.last().is_some_and(|part|part.offset+part.length>cursor));
}
#[cfg(test)] mod covered_tests {
 use super::*;
 #[test] fn committing_a_longer_suffix_drops_the_covered_primary_chunk() {
  let part=|offset,length|TilePart {tile:0,offset,length,ordinal:0,total:1};
  let mut lanes=vec![Lane {parts:vec![part(100,20),part(120,20)],cursor:140},Lane {parts:vec![part(80,20),part(100,20),part(120,20),part(140,20)],cursor:160},Lane {parts:vec![part(200,20)],cursor:220}];
  let parts=connected(&mut lanes,100).unwrap();assert_eq!(parts.len(),3);prune_covered(&mut lanes,160);
  assert_eq!(lanes.len(),1);assert_eq!(lanes[0].parts[0].offset,200);
 }
}

fn contains_requested(lane:&Lane,wanted:Option<&HashSet<u16>>)->bool {
 wanted.is_some_and(|tiles|lane.parts.iter().any(|part|tiles.contains(&part.tile)))
}
#[cfg(test)] mod partial_stop_tests {
 use super::*;
 #[test] fn a_retained_lane_stops_even_when_the_requested_part_is_not_last() {
  let part=|tile,offset|TilePart {tile,offset,length:20,ordinal:0,total:1};
  let lane=Lane {parts:vec![part(11,100),part(12,120),part(13,140)],cursor:160};
  assert!(contains_requested(&lane,Some(&HashSet::from([12]))));
  assert!(!contains_requested(&lane,Some(&HashSet::from([14]))));
  assert!(!contains_requested(&lane,None));
 }
}

// Use observed nontrivial part lengths to avoid many windows too narrow to
// find a seed. The byte budget is unchanged. Two source lanes remain available
// for the required walk and other reads; partial views keep their old frontier.
fn learned_shard_count(default:usize,whole:bool,latency:u64,frontier:usize,budget:u64,parts:&[u64],adaptive:bool)->usize {
 if !adaptive || !whole || !(5000..u64::MAX).contains(&latency) || frontier<8 || parts.len()<3 {return default;}
 let largest=parts.iter().copied().max().unwrap_or(0);
 if largest<64*1024 {return default;}
 let capacity=frontier.saturating_sub(2).min(16);
 budget.div_ceil(largest).clamp(4,capacity as u64) as usize
}
#[cfg(test)] mod learned_seed_tests {
 use super::*;
 #[test] fn observed_large_parts_bound_the_frontier_inside_the_same_byte_budget() {
  let sizes=[3_101_612,3_055_701,3_002_316];
  assert_eq!(learned_shard_count(4,true,16000,8,16*1024*1024-196608,&sizes,true),6);
  assert_eq!(learned_shard_count(4,true,16000,16,16*1024*1024-196608,&sizes,true),6);
  assert_eq!(learned_shard_count(4,true,16000,8,4*1024*1024,&sizes,true),4);
  assert_eq!(learned_shard_count(4,true,16000,8,16*1024*1024,&[8*1024*1024;3],true),4);
 }
 #[test] fn partial_nearby_observe_and_uncalibrated_sources_keep_their_frontier() {
  let sizes=[3_000_000;3];let budget=16*1024*1024;
  for latency in [0,1000,4999,u64::MAX] {assert_eq!(learned_shard_count(4,true,latency,8,budget,&sizes,true),4);}
  assert_eq!(learned_shard_count(4,false,16000,8,budget,&sizes,true),4);
  assert_eq!(learned_shard_count(16,true,16000,4,budget,&sizes,true),16);
  assert_eq!(learned_shard_count(4,true,16000,8,budget,&sizes,false),4);
  assert_eq!(learned_shard_count(4,true,16000,8,budget,&sizes[..2],true),4);
 }
 # [test] fn fine_parts_cannot_occupy_every_source_lane() {
  assert_eq!(learned_shard_count(4,true,16000,8,16*1024*1024,&[65536;3],true),6);
  assert_eq!(learned_shard_count(4,true,16000,16,16*1024*1024,&[65536;3],true),14);
 }
}
