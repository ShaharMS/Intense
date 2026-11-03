//! A large physical range with bounded retention of marker-head blocks.
//! This is a prefetch only: the ordinary parser validates every marker again.
use super::*;
pub(super) const RETAINED_LIMIT:usize=256*1024;
struct Skimmer {block_size:usize,offset:u64,next:u64,probe:Vec<u8>,pending:Vec<u8>,done:bool,blocks:HashMap<u64,Arc<[u8]>>,retained:usize,body_bytes:u64,body_start:Option<u64>}
impl Skimmer {
 fn new(start:u64,marker:u64,block_size:u64,body_bytes:u64)->Self {Self {block_size:block_size as usize,offset:start,next:marker,probe:vec![],pending:Vec::with_capacity(block_size as usize),done:false,blocks:HashMap::new(),retained:0,body_bytes,body_start:None}}
 fn feed(&mut self,mut bytes:&[u8]) {while !bytes.is_empty() {let n=bytes.len().min(self.block_size-self.pending.len());self.pending.extend_from_slice(&bytes[..n]);bytes=&bytes[n..];if self.pending.len()==self.block_size {let mut block=std::mem::take(&mut self.pending);self.block(&block);self.offset+=block.len() as u64;block.clear();self.pending=block;}}}
 fn block(&mut self,bytes:&[u8]) {
  let end=self.offset+bytes.len() as u64;let mut keep=false;
  while !self.done {
   let cursor=self.next+self.probe.len() as u64;if cursor>=end {break;}if cursor<self.offset {self.done=true;break;}
   let at=(cursor-self.offset) as usize;let n=(5-self.probe.len()).min(bytes.len()-at);self.probe.extend_from_slice(&bytes[at..at+n]);keep=true;if self.probe.len()<5 {break;}
   let code=u16::from_be_bytes([self.probe[0],self.probe[1]]);let length=u16::from_be_bytes([self.probe[2],self.probe[3]]) as u64;
   if code==0xff93 {self.body_start=Some(self.next+2);self.done=true;}else if length<2 {self.done=true;}else {self.next=self.next.saturating_add(length+2);self.probe.clear();}
  }
  keep|=self.body_start.is_some_and(|start|self.offset<start.saturating_add(self.body_bytes) && end>start && self.body_bytes>0);
  // Never cache a short block in the middle of an object: later reads may
  // legitimately need the remainder (including the next tile-part SOT).
  if keep && bytes.len()==self.block_size && self.retained+bytes.len()<=RETAINED_LIMIT {self.retained+=bytes.len();self.blocks.insert(self.offset,Arc::from(bytes));}
 }
 fn finish(mut self)->HashMap<u64,Arc<[u8]>> {if !self.pending.is_empty() {let b=std::mem::take(&mut self.pending);self.block(&b);}self.blocks}
}
impl RangeClient {
 async fn fetch_header_heads(&self,range:Range<u64>,marker:u64,body_bytes:u64,deadline:Instant,mut cancel:Option<watch::Receiver<bool>>)->Result<HashMap<u64,Arc<[u8]>>> {
  let mut trace=read_trace::Read::begin(self.trace_id,&self.object_id,self.caller,"headerWindow",range.clone(),self.block_size,None,self.part,"sdk");
  let opts=GetOptions {range:Some(GetRange::Bounded(range.clone())),if_match:self.etag.clone(),version:self.version.clone(),..Default::default()};
  let get=async {
   let _permit=self.profile.acquire().await;ensure!(Instant::now()<deadline,"header window source admission deadline exceeded");
   let network=async {
    let started=Instant::now();let result=self.budget.scope(trace.scope(self.store.get_opts(&self.location,opts))).await?;ensure!(result.range==range,"S3 endpoint changed header window range");let headers=started.elapsed();let mut stream=result.into_stream();let mut seen=0u64;let mut skim=Skimmer::new(range.start,marker,self.block_size,body_bytes);
    while let Some(chunk)=stream.try_next().await? {ensure!(seen+chunk.len() as u64<=range.end-range.start,"header window response exceeds range");seen+=chunk.len() as u64;skim.feed(&chunk);}
    ensure!(seen==range.end-range.start,"incomplete header window response");self.profile.record(seen,headers,started.elapsed());self.network_min_us.fetch_min(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64,Ordering::Relaxed);Ok::<_,anyhow::Error>(skim.finish())
   };
   tokio::time::timeout(Duration::from_secs(10).min(deadline.saturating_duration_since(Instant::now())),network).await.context("header window network timeout")?
  };
  let cancelled=async {if let Some(r)=&mut cancel {if !*r.borrow() {let _=r.changed().await;}}else {std::future::pending::<()>().await;}};
  let result=tokio::select! {r=tokio::time::timeout(deadline.saturating_duration_since(Instant::now()),get)=>r.context("header window deadline exceeded").and_then(|r|r),_=cancelled=>Err(anyhow::anyhow!("S3 request superseded"))};
  trace.finish(if result.is_ok(){range.end-range.start}else{0},result.is_ok(),None);result
 }
}
impl RemoteSource {
 pub(super) fn prefetch_streamed_header(&self,marker:u64,end:u64,context:&mut ReadContext,body_bytes:u64)->Result<()> {
  context.check()?;let start=marker/self.block_size*self.block_size;ensure!(end>start && end<=self.meta.size && end-start<=8*1024*1024,"invalid streamed header window");ensure!(context.stats.bytes+end-start<=context.limit,"streamed header window exceeds compressed-read budget");ensure!(context.stats.requests<context.requests_limit,"streamed header window exceeds GET budget");
  let _fetch=self.fetch_admission(context)?;let client=self.range_client(context);let blocks=self.runtime.block_on(client.fetch_header_heads(start..end,marker,body_bytes,context.deadline,context.cancel.clone()))?;context.stats.bytes+=end-start;context.stats.requests+=1;
  // Keep sparse leases even with the shared cache disabled. No PLT body is
  // admitted to the shared cache merely because it passed through the stream.
  for (&at,bytes) in &blocks {self.cache.put(&self.cache_key,self.block_size,at,bytes);}
  context.pin_packet_blocks(blocks);Ok(())
 }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn marker_blocks_are_chunk_independent_and_payload_signatures_are_not_trusted() {
  let mut bytes=vec![0;1024];let first=1021usize;bytes.resize(first,0);bytes.extend([0xff,0x58,0xff,0xff,0]);bytes.resize(first+65537,0x11);bytes[2048..2053].copy_from_slice(&[0xff,0x93,0,2,0]);let second=bytes.len();bytes.extend([0xff,0x52,0,4,0,0]);let third=bytes.len();bytes.extend([0xff,0x93,0,0,0]);bytes.resize(bytes.len().div_ceil(1024)*1024,0);
  let mut expected=None;for chunk in [1,3,1023,1024,4096,65536] {let mut skim=Skimmer::new(0,first as u64,1024,0);for b in bytes.chunks(chunk) {skim.feed(b);}let blocks=skim.finish();assert!(blocks.contains_key(&(second as u64/1024*1024)));assert!(blocks.contains_key(&(third as u64/1024*1024)));assert!(!blocks.contains_key(&2048));let mut keys=blocks.keys().copied().collect::<Vec<_>>();keys.sort();if let Some(e)=&expected {assert_eq!(&keys,e);}else {expected=Some(keys);}}
 }
 #[test] fn requested_body_prefix_can_be_reused_without_retaining_the_tail() {let mut bytes=vec![0xff,0x93,1,2,3];bytes.resize(8192,0x33);let mut skim=Skimmer::new(0,0,1024,1500);for chunk in bytes.chunks(77) {skim.feed(chunk);}let blocks=skim.finish();assert_eq!(blocks.len(),2);assert!(blocks.contains_key(&0) && blocks.contains_key(&1024));assert!(!blocks.contains_key(&2048));}
 #[test] fn retention_is_bounded_and_short_tail_blocks_are_not_cached() {let bytes=[0xff,0x64,0,4,0,0].repeat(200000);let mut skim=Skimmer::new(0,0,1024,0);for b in bytes.chunks(8192) {skim.feed(b);}let blocks=skim.finish();assert!(blocks.values().map(|b|b.len()).sum::<usize>()<=RETAINED_LIMIT);assert!(blocks.values().all(|b|b.len()==1024));}
}
