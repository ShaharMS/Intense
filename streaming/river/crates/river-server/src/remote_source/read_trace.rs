//! Bounded diagnostic records. Never retain URLs, keys, headers or error text.
use std::{collections::{VecDeque,BTreeMap}, ops::Range, sync::{Arc, Mutex, OnceLock, atomic::{AtomicBool, AtomicU64, Ordering}}, time::{Duration,Instant}};
use serde::Serialize;
tokio::task_local! {static CURRENT:Arc<Event>;}
const CAPACITY:usize=4096;
static ENABLED:OnceLock<AtomicBool>=OnceLock::new();
static SEQUENCE:AtomicU64=AtomicU64::new(1);
static CONTEXT:AtomicU64=AtomicU64::new(1);
fn enabled()->bool {ENABLED.get_or_init(||AtomicBool::new(std::env::var("RIVER_TRACE_SOURCE_READS").as_deref()==Ok("1"))).load(Ordering::Relaxed)}
pub fn configure(value:bool) {ENABLED.get_or_init(||AtomicBool::new(false)).store(value,Ordering::Relaxed);}
pub fn context_id()->u64 {CONTEXT.fetch_add(1,Ordering::Relaxed)}
#[derive(Clone,Serialize)]
#[serde(rename_all="camelCase")]
pub struct Event {
 pub kind:&'static str,pub sequence:u64,pub context_id:u64,pub object_id:String,pub caller:String,pub purpose:&'static str,
 pub start:u64,pub end_exclusive:u64,pub block_index:u64,pub block_size:u64,
 pub tile:Option<u16>,pub tile_part:Option<usize>,pub transport:&'static str,
 #[serde(skip_serializing_if="Option::is_none")] pub admission_ms:Option<f64>,
 #[serde(skip_serializing_if="Option::is_none")] pub headers_ms:Option<f64>,
 #[serde(skip_serializing_if="Option::is_none")] pub body_ms:Option<f64>,
 pub elapsed_ms:f64,pub bytes:u64,pub outcome:&'static str,
 /// SDK may retry internally; these records explicitly do not claim wire counts.
 pub physical_attempts:Option<u64>,pub http_status:Option<u16>,
}
#[derive(Default)]struct Buffer {events:VecDeque<Event>,evicted:u64}
fn buffer()->&'static Mutex<Buffer> {static B:OnceLock<Mutex<Buffer>>=OnceLock::new();B.get_or_init(||Mutex::new(Buffer::default()))}
#[derive(Default,Serialize)]
#[serde(rename_all="camelCase")]
struct GroupSummary {context_id:u64,object_id:String,purpose:String,caller:String,logical_reads:u64,physical_attempts:u64,body_bytes:u64,index_entries:u64,http_statuses:BTreeMap<u16,u64>}
fn summarize(events:&VecDeque<Event>,evicted:u64)->serde_json::Value {
 let mut groups=BTreeMap::<(u64,String,String,String),GroupSummary>::new();
 let mut logical=0;let mut physical=0;let mut bytes=0;let mut indices=0;
 for e in events {
  let key=(e.context_id,e.object_id.clone(),e.purpose.to_owned(),e.caller.clone());
  let g=groups.entry(key).or_insert_with(||GroupSummary {context_id:e.context_id,object_id:e.object_id.clone(),purpose:e.purpose.into(),caller:e.caller.clone(),..Default::default()});
  if e.kind=="wireAttempt" || e.purpose=="nativeAttempt" {
   let n=e.physical_attempts.unwrap_or(0);g.physical_attempts+=n;physical+=n;
   if let Some(status)=e.http_status {*g.http_statuses.entry(status).or_default()+=1;}
  }
  if (e.kind=="rangeRead" && e.purpose!="nativeAttempt") || e.kind=="logicalRead" {g.logical_reads+=1;logical+=1;}
  // SDK body bytes live on the logical record; native bytes on each attempt.
  if (e.kind=="rangeRead" && e.purpose!="nativeAttempt") || e.purpose=="nativeAttempt" {g.body_bytes+=e.bytes;bytes+=e.bytes;}
  if e.kind=="indexEntry" {g.index_entries+=1;indices+=1;}
 }
 serde_json::json!({"partial":evicted>0,"logicalReads":logical,"physicalAttempts":physical,"recordedBodyBytes":bytes,"validatedIndexEntries":indices,"groups":groups.into_values().collect::<Vec<_>>(),
  "scope":"Retained records only; incomplete/dropped records may have unknown attempt or body-byte counts. Attempts are transport sends, not proof a GET reached the origin. SDK wire records stop at headers. Native logical parents are excluded from physical and byte totals. Index entries are not GETs. Optional phase timings belong to individual records and are not summed as wall latency. Logical SDK header time includes retries/backoff; wireAttempt header time covers one send. Admission time ends when a source permit is obtained; unknown phases are omitted."})
}
pub fn snapshot()->serde_json::Value {
 let b=buffer().lock().unwrap_or_else(|e|e.into_inner());
 serde_json::json!({"enabled":enabled(),"capacity":CAPACITY,"evicted":b.evicted,"events":b.events,
 "summary":summarize(&b.events,b.evicted),
 "scope":"Uncached River range operations. rangeRead records are logical operations; wireAttempt records show each SDK HTTP send attempt, including retries, through response headers. Curl nativeAttempt records include the body. Connect failures may occur before a GET reaches the source. Initial object probe is a separate context. End offsets are exclusive. Incomplete operations include errors and dropped/cancelled futures; they are not successes. No URLs, object keys, credentials or error bodies are retained."})
}
#[track_caller]
pub fn index_entry(context:u64,object:&str,part:usize,tile:u16,range:Range<u64>,block:u64) {
 index_entry_from(context,object,part,tile,range,block,"validatedSot");
}
#[track_caller]
pub fn index_entry_from(context:u64,object:&str,part:usize,tile:u16,range:Range<u64>,block:u64,purpose:&'static str) {
 let mut r=Read::begin(context,object,std::panic::Location::caller(),purpose,range,block,Some(tile),Some(part),"none");
 if let Some(e)=&mut r.event {Arc::make_mut(e).kind="indexEntry";}r.finish(0,true,Some(0));
}
pub fn clear() {let mut b=buffer().lock().unwrap_or_else(|e|e.into_inner());b.events.clear();b.evicted=0;}
pub struct Read {event:Option<Arc<Event>>,started:Instant}
impl Read {
 pub fn begin(context:u64,object:&str,caller:&std::panic::Location<'_>,purpose:&'static str,range:Range<u64>,block:u64,tile:Option<u16>,part:Option<usize>,transport:&'static str)->Self {
  let event=enabled().then(||Arc::new(Event {kind:"rangeRead",sequence:SEQUENCE.fetch_add(1,Ordering::Relaxed),context_id:context,
   object_id:object.to_owned(),caller:format!("{}:{}",caller.file(),caller.line()),purpose,start:range.start,end_exclusive:range.end,
   block_index:range.start/block.max(1),block_size:block,tile,tile_part:part,transport,elapsed_ms:0.0,bytes:0,outcome:"incompleteOrCancelled",physical_attempts:None,http_status:None,admission_ms:None,headers_ms:None,body_ms:None}));
  Self {event,started:Instant::now()}
 }
 pub async fn scope<F:std::future::Future>(&self,future:F)->F::Output {if let Some(e)=&self.event {CURRENT.scope(e.clone(),future).await}else{future.await}}
 pub fn wire()->Self {
  let event=if enabled() {CURRENT.try_with(|e| {let mut e=(**e).clone();e.admission_ms=None;e.headers_ms=None;e.body_ms=None;e.kind="wireAttempt";e.sequence=SEQUENCE.fetch_add(1,Ordering::Relaxed);e.transport="sdk";e.bytes=0;e.outcome="incompleteOrCancelled";e.physical_attempts=Some(1);Arc::new(e)}).ok()}else{None};
  Self {event,started:Instant::now()}
 }
 pub fn headers(&mut self,status:Option<u16>,ok:bool) {self.finish(0,ok,Some(1));if let Some(e)=&mut self.event {let e=Arc::make_mut(e);e.headers_ms=status.map(|_|self.started.elapsed().as_secs_f64()*1000.0);e.http_status=status;if status.is_some_and(|s|s>=400){e.outcome="httpError";}}}
 pub fn native_parent(&mut self) {if let Some(e)=&mut self.event {let e=Arc::make_mut(e);e.kind="logicalRead";e.transport="curl";}}
 pub fn is_recording(&self)->bool {self.event.is_some()}
 pub fn admitted(&mut self) {if let Some(e)=&mut self.event {Arc::make_mut(e).admission_ms=Some(self.started.elapsed().as_secs_f64()*1000.0);}}
 /// Logical SDK transfer, including SDK retry/backoff in the header interval.
 /// Native transfers retain unknown phases instead of inventing boundaries.
 pub fn transfer_timing(&mut self,headers:Duration,total:Duration) {if let Some(e)=&mut self.event {
  let e=Arc::make_mut(e);e.headers_ms=Some(headers.as_secs_f64()*1000.0);
  e.body_ms=total.checked_sub(headers).map(|d|d.as_secs_f64()*1000.0);
 }}
 pub fn finish(&mut self,bytes:u64,ok:bool,attempts:Option<u64>) {if let Some(e)=&mut self.event {let e=Arc::make_mut(e);e.bytes=bytes;e.outcome=if ok {"ok"} else {"error"};e.physical_attempts=attempts;}}
}
impl Drop for Read {fn drop(&mut self) {if let Some(event)=self.event.take() {
 let mut event=Arc::unwrap_or_clone(event);
 event.elapsed_ms=self.started.elapsed().as_secs_f64()*1000.0;
 let mut b=buffer().lock().unwrap_or_else(|e|e.into_inner());if b.events.len()==CAPACITY {b.events.pop_front();b.evicted+=1;}b.events.push_back(event);
}}}
#[cfg(test)]mod tests {
 use super::*;
 #[test]fn summary_does_not_double_count_native_parents_or_index_records() {
  let base=Event {kind:"rangeRead",sequence:1,context_id:7,object_id:"opaque".into(),caller:"source.rs:9".into(),purpose:"headerOrPacket",start:0,end_exclusive:256,block_index:0,block_size:256,tile:None,tile_part:None,transport:"sdk",elapsed_ms:1.0,bytes:256,outcome:"ok",physical_attempts:None,http_status:None,admission_ms:None,headers_ms:None,body_ms:None};
  let mut records=VecDeque::from([base.clone()]);
  for status in [503,206] {let mut e=base.clone();e.kind="wireAttempt";e.bytes=0;e.http_status=Some(status);e.physical_attempts=Some(1);records.push_back(e);}
  let mut parent=base.clone();parent.kind="logicalRead";parent.transport="curl";parent.physical_attempts=Some(2);records.push_back(parent);
  for bytes in [0,256] {let mut e=base.clone();e.purpose="nativeAttempt";e.bytes=bytes;e.physical_attempts=Some(1);records.push_back(e);}
  for purpose in ["validatedSot","validatedTlmSot","validatedConnectedSot"] {
   let mut index=base.clone();index.kind="indexEntry";index.purpose=purpose;index.bytes=0;records.push_back(index);
  }
  let s=summarize(&records,0);assert_eq!(s["logicalReads"],2);assert_eq!(s["physicalAttempts"],4);assert_eq!(s["recordedBodyBytes"],512);assert_eq!(s["validatedIndexEntries"],3);assert_eq!(s["partial"],false);
  assert_eq!(summarize(&records,1)["partial"],true);
 }
 #[test]fn bounded_records_keep_indices_outcomes_and_disclose_eviction() {
  // Test a private buffer to avoid changing process-wide tracing during other tests.
  let mut b=Buffer::default();
  for n in 0..CAPACITY+3 {let mut r=Read {event:Some(Arc::new(Event {kind:"rangeRead",sequence:n as u64,context_id:7,object_id:"opaque".into(),caller:"source.rs:9".into(),purpose:"sotDiscovery",start:512,end_exclusive:768,block_index:2,block_size:256,tile:Some(4),tile_part:Some(8),transport:"sdk",elapsed_ms:0.0,bytes:0,outcome:"incompleteOrCancelled",physical_attempts:None,http_status:None,admission_ms:None,headers_ms:None,body_ms:None})),started:Instant::now()};r.finish(256,true,None);let e=r.event.take().unwrap();if b.events.len()==CAPACITY {b.events.pop_front();b.evicted+=1;}b.events.push_back(Arc::unwrap_or_clone(e));}
  assert_eq!(b.events.len(),CAPACITY);assert_eq!(b.evicted,3);assert_eq!(b.events.front().unwrap().sequence,3);
  let e=b.events.back().unwrap();assert_eq!((e.block_index,e.tile,e.tile_part,e.bytes),(2,Some(4),Some(8),256));assert!(e.physical_attempts.is_none());
 }
 fn fixture()->Read {
  Read {event:Some(Arc::new(Event {kind:"rangeRead",sequence:0,context_id:0,object_id:"opaque".into(),caller:"fixture".into(),purpose:"fixture",start:0,end_exclusive:1,block_index:0,block_size:1,tile:None,tile_part:None,transport:"sdk",elapsed_ms:0.0,bytes:0,outcome:"incompleteOrCancelled",physical_attempts:None,http_status:None,admission_ms:None,headers_ms:None,body_ms:None})),started:Instant::now()}
 }
 #[tokio::test]async fn scope_shares_context_and_finalization_keeps_retained_snapshot_immutable() {
  let mut r=fixture();let retained=r.scope(async {CURRENT.with(Arc::clone)}).await;
  assert!(Arc::ptr_eq(r.event.as_ref().unwrap(),&retained));
  r.transfer_timing(Duration::from_millis(2),Duration::from_millis(5));
  assert!(retained.headers_ms.is_none());let result=r.event.take().unwrap();
  assert_eq!(result.headers_ms,Some(2.0));assert_eq!(result.body_ms,Some(3.0));
  assert!(std::mem::size_of::<Read>()<=32);
  eprintln!("TRACE_CONTEXT_BYTES {}",std::mem::size_of::<Read>());
 }
 #[test]fn timing_boundaries_omit_unknown_or_invalid_phases() {
  let mut r=fixture();r.transfer_timing(Duration::from_millis(5),Duration::from_millis(2));
  assert!(r.event.as_ref().unwrap().body_ms.is_none());
  r.headers(None,false);let event=r.event.take().unwrap();
  assert!(event.headers_ms.is_none());assert_eq!(event.outcome,"error");
 }

}
