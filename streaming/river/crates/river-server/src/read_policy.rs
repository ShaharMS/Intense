//! Per-connection observations; no probes, credentials, image bytes or disk IO.
use std::{collections::BTreeMap,sync::{Arc,Mutex,OnceLock},time::Duration};
use anyhow::{Result,ensure};
pub const THROUGHPUT_SAMPLE_BYTES:u64=32*1024;
pub const MAX_CONTIGUOUS_RANGE_BYTES:u64=4*1024*1024;
pub const CONTIGUOUS_BODY_BUDGET_MS:u32=125;
#[derive(Clone,Copy,Debug,PartialEq,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub enum Mode {Disabled,Observe,Adaptive}
#[derive(Clone,Copy,Debug,PartialEq,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub enum SerialTransport {Sdk,Curl,Auto}
impl Default for SerialTransport {fn default()->Self {match std::env::var("RIVER_S3_SERIAL_TRANSPORT").as_deref(){Ok("curl")=>Self::Curl,Ok("auto")=>Self::Auto,_=>Self::Sdk}}}
#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Settings {#[serde(default,skip_serializing_if="Option::is_none")]pub index_headers:Option<IndexHeaders>,#[serde(default,skip_serializing_if="Option::is_none")]pub max_source_requests:Option<usize>,pub mode:Mode,pub minimum_samples:u32,pub max_overfetch_ratio:u32,#[serde(default,skip_serializing_if="Option::is_none")]pub serial_transport:Option<SerialTransport>,#[serde(default,skip_serializing_if="Option::is_none")]pub refresh_payload_costs:Option<bool>}
impl Default for Settings {fn default()->Self {Self {index_headers:None,max_source_requests:None,mode:match std::env::var("RIVER_S3_ADAPTIVE_READS").unwrap_or_else(|_|"observe".into()).as_str(){"true"|"adaptive"=>Mode::Adaptive,"false"|"disabled"=>Mode::Disabled,_=>Mode::Observe},minimum_samples:16,max_overfetch_ratio:2,serial_transport:None,refresh_payload_costs:None}}}
impl Settings {
 pub fn effective_refresh_payload_costs(&self)->bool {self.refresh_payload_costs.unwrap_or_else(||matches!(std::env::var("RIVER_S3_REFRESH_PAYLOAD_COSTS").as_deref(),Ok("true"|"1")))}
 pub fn effective_source_requests(&self)->usize {self.max_source_requests.unwrap_or_else(||std::env::var("RIVER_S3_SOURCE_MAX_REQUESTS").ok().and_then(|v|v.parse().ok()).unwrap_or(16).clamp(1,64))}
 pub fn effective_serial_transport(&self)->SerialTransport {self.serial_transport.unwrap_or_default()}
 pub fn validate(&self)->Result<()> {if let Some(p)=&self.index_headers {p.validate()?;}ensure!(self.max_source_requests.is_none_or(|n|(1..=64).contains(&n)),"maxSourceRequests must be 1 through 64");ensure!((8..=10000).contains(&self.minimum_samples),"minimumSamples must be 8 through 10000");ensure!((1..=4).contains(&self.max_overfetch_ratio),"maxOverfetchRatio must be 1 through 4");crate::remote_source::validate_transport_kind(self.effective_serial_transport())}
}
pub fn settings_view(settings:&Settings)->serde_json::Value {let mut v=serde_json::to_value(settings).unwrap();v["maxSourceRequests"]=settings.effective_source_requests().into();v["serialTransport"]=serde_json::to_value(settings.effective_serial_transport()).unwrap();v["refreshPayloadCosts"]=settings.effective_refresh_payload_costs().into();v}
static SETTINGS:OnceLock<Mutex<Settings>>=OnceLock::new();
pub fn explicit_overfetch_cap()->Option<u64> {std::env::var("RIVER_S3_RANGE_OVERFETCH_RATIO").ok().and_then(|v|v.parse::<u64>().ok()).map(|v|v.clamp(1,16))}
pub fn settings()->Settings {SETTINGS.get_or_init(||Mutex::new(Settings::default())).lock().unwrap_or_else(|e|e.into_inner()).clone()}
pub fn apply(value:Settings) {*SETTINGS.get_or_init(||Mutex::new(Settings::default())).lock().unwrap_or_else(|e|e.into_inner())=value;}
#[derive(Debug,Default,Clone,serde::Serialize)]
#[serde(rename_all="camelCase")]
pub struct Observation {#[serde(skip_serializing_if="Option::is_none")]pub contiguous_ranges:Option<ContiguousDecision>,#[serde(skip_serializing_if="Option::is_none")]pub last_body_bytes_per_second:Option<f64>,#[serde(skip_serializing_if="Option::is_none")]pub index_header_decision:Option<IndexHeaderDecision>,pub active_reads:u64,pub active_preparations:u64,pub admission_waits:u64,pub samples:u64,pub bytes:u64,pub latency_ms:Option<f64>,pub effective_bytes_per_second:Option<f64>,pub throughput_samples:u64,pub body_bytes_per_second:Option<f64>,pub body_throughput_samples:u64,pub sdk_serial_reads:u64,pub native_serial_requests:u64,pub last_decision:Option<Decision>}
#[derive(Debug,Clone,serde::Serialize)]
#[serde(rename_all="camelCase")]
pub struct ContiguousDecision {pub reason:String,pub max_range_bytes:u64,pub body_time_budget_ms:u32,pub sample_count:u64,pub body_throughput_samples:u64,pub gap_bytes_added:u64}
pub fn contiguous_decision(settings:&Settings,o:&Observation)->ContiguousDecision {
 let mut d=ContiguousDecision {reason:"conservativeDefault".into(),max_range_bytes:1024*1024,body_time_budget_ms:CONTIGUOUS_BODY_BUDGET_MS,sample_count:o.samples,body_throughput_samples:o.body_throughput_samples,gap_bytes_added:0};
 if settings.mode!=Mode::Adaptive {d.reason="learningNotApplied".into();return d;}
 if o.samples<u64::from(settings.minimum_samples) || o.body_throughput_samples<3 {d.reason="insufficientMeasurements".into();return d;}
 let (Some(average),Some(latest))=(o.body_bytes_per_second,o.last_body_bytes_per_second) else {d.reason="missingBodyThroughput".into();return d;};
 if !average.is_finite() || !latest.is_finite() {return d;}
 let rate=average.min(latest);if rate<=0.0 {return d;}
 d.max_range_bytes=((rate*(f64::from(CONTIGUOUS_BODY_BUDGET_MS)/1000.0)) as u64).clamp(1024*1024,MAX_CONTIGUOUS_RANGE_BYTES)/1024*1024;
 d.reason=if d.max_range_bytes>1024*1024 {"requiredContiguousBytes"}else{"bodyTransferBudget"}.into();d
}
#[derive(Debug,Clone,serde::Serialize)]
#[serde(rename_all="camelCase")]
pub struct Decision {pub reason:String,pub sample_count_at_decision:u64,pub throughput_samples_at_decision:u64,pub original_requests:usize,pub planned_requests:usize,pub original_bytes:u64,pub planned_bytes:u64,pub estimated_original_ms:f64,pub estimated_planned_ms:f64,pub applied:bool}
#[derive(Debug,Default)] pub struct Profile(Mutex<Observation>,std::sync::atomic::AtomicUsize,tokio::sync::Notify,std::sync::atomic::AtomicUsize);
pub struct SourcePermit<'a>(&'a Profile);
pub struct SourcePreparation<'a>(&'a Profile);
impl Drop for SourcePreparation<'_> {fn drop(&mut self){self.0.3.fetch_sub(1,std::sync::atomic::Ordering::AcqRel);}}
impl Observation {
 pub fn planning_bytes_per_second(&self)->Option<f64> {
  let valid=|value:Option<f64>|value.filter(|rate|rate.is_finite() && *rate>0.0);
  if self.body_throughput_samples>=3 {
   let average=valid(self.body_bytes_per_second).or_else(||valid(self.effective_bytes_per_second));
   match (average,valid(self.last_body_bytes_per_second)) {
    (Some(average),Some(latest))=>Some(average.min(latest)),
    (average,_)=>average,
   }
  }else{valid(self.effective_bytes_per_second)}
 }
}
impl Drop for SourcePermit<'_> {fn drop(&mut self) {self.0.1.fetch_sub(1,std::sync::atomic::Ordering::AcqRel);self.0.2.notify_waiters();}}
type Profiles=BTreeMap<String,Arc<Profile>>;
fn profiles()->&'static Mutex<Profiles> {static VALUE:OnceLock<Mutex<Profiles>>=OnceLock::new();VALUE.get_or_init(||Mutex::new(BTreeMap::new()))}
pub fn profile(id:&str)->Arc<Profile> {let mut all=profiles().lock().unwrap_or_else(|e|e.into_inner());if let Some(p)=all.get(id){return p.clone();}let p=Arc::new(Profile::default());// Admission identity must remain shared for every connected source. Connections
// already own clients/configuration; entries are removed on replacement/deletion.
all.insert(id.to_owned(),p.clone());p}
pub fn reset(id:&str) {profiles().lock().unwrap_or_else(|e|e.into_inner()).remove(id);}
pub fn reset_all() {profiles().lock().unwrap_or_else(|e|e.into_inner()).clear();}
pub fn observations()->BTreeMap<String,Observation> {profiles().lock().unwrap_or_else(|e|e.into_inner()).iter().map(|(id,p)|(id.clone(),p.snapshot())).collect()}
impl Profile {
 pub fn active_operations(&self)->usize {self.1.load(std::sync::atomic::Ordering::Acquire)}
 pub fn active_preparations(&self)->usize {self.3.load(std::sync::atomic::Ordering::Acquire)}
 pub fn preparation(&self)->SourcePreparation<'_> {self.3.fetch_add(1,std::sync::atomic::Ordering::AcqRel);SourcePreparation(self)}
 pub async fn acquire(&self)->SourcePermit<'_> {use std::sync::atomic::Ordering;
  let mut queued=false;
  loop {let notified=self.2.notified();tokio::pin!(notified);notified.as_mut().enable();
   let limit=settings().effective_source_requests();
   // Retry a racing update while capacity remains; a stale CAS result is not
   // evidence that the source is full and must never impose a100ms queue wait.
   if self.1.fetch_update(Ordering::AcqRel,Ordering::Acquire,|active|(active<limit).then_some(active+1)).is_ok() {return SourcePermit(self);}
   if !queued {queued=true;let mut s=self.0.lock().unwrap_or_else(|e|e.into_inner());s.admission_waits=s.admission_waits.saturating_add(1);}
   // Also recheck live configuration changes without waiting for another completion.
   tokio::select! {_=notified=>{},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
  }
 }

 pub fn serial_counts(&self,sdk_reads:u64,native_requests:u64) {let mut s=self.0.lock().unwrap_or_else(|e|e.into_inner());s.sdk_serial_reads=s.sdk_serial_reads.saturating_add(sdk_reads);s.native_serial_requests=s.native_serial_requests.saturating_add(native_requests);}
 pub fn snapshot(&self)->Observation {let mut s=self.0.lock().unwrap_or_else(|e|e.into_inner()).clone();s.active_reads=self.active_operations() as u64;s.active_preparations=self.active_preparations() as u64;s}
 pub fn record(&self,bytes:u64,headers:Duration,total:Duration) {
  self.record_transfer(bytes,Some(headers),total);
 }
 pub fn record_transfer(&self,bytes:u64,headers:Option<Duration>,total:Duration) {
  if settings().mode==Mode::Disabled {return;}
  let mut s=self.0.lock().unwrap_or_else(|e|e.into_inner());s.samples=s.samples.saturating_add(1);s.bytes=s.bytes.saturating_add(bytes);
  let ewma=|old:Option<f64>,new:f64|Some(old.map_or(new,|old|old*0.875+new*0.125));
  if let Some(headers)=headers {s.latency_ms=ewma(s.latency_ms,headers.as_secs_f64()*1000.0);}
  if bytes>=THROUGHPUT_SAMPLE_BYTES && total.as_secs_f64()>0.0 {s.throughput_samples=s.throughput_samples.saturating_add(1);s.effective_bytes_per_second=ewma(s.effective_bytes_per_second,bytes as f64/total.as_secs_f64());}
  // The planner adds header latency separately. Keep the legacy whole-request
  // rate for diagnostics/fallback, but do not charge that latency again when
  // the transport supplies a measured header/body boundary.
  if bytes>=THROUGHPUT_SAMPLE_BYTES {
   if let Some(body)=headers.and_then(|h|total.checked_sub(h)).filter(|d|!d.is_zero()) {
    s.body_throughput_samples=s.body_throughput_samples.saturating_add(1);
    let rate=bytes as f64/body.as_secs_f64();s.last_body_bytes_per_second=Some(rate);
    s.body_bytes_per_second=ewma(s.body_bytes_per_second,rate);
   }
  }
 }
 pub fn decision(&self,value:Decision) {self.0.lock().unwrap_or_else(|e|e.into_inner()).last_decision=Some(value);}
 pub fn contiguous_decision(&self,value:ContiguousDecision) {self.0.lock().unwrap_or_else(|e|e.into_inner()).contiguous_ranges=Some(value);}
}
pub fn estimated_ms(requests:usize,bytes:u64,concurrency:usize,latency_ms:f64,bytes_per_second:f64)->f64 {
 requests.div_ceil(concurrency.max(1)) as f64*latency_ms+bytes as f64/bytes_per_second.max(1.0)*1000.0
}
#[cfg(test)] mod tests {
 #[test] fn contiguous_chunks_need_learning_and_back_off_to_the_latest_slow_transfer() {
  let mut s=Settings::default();s.mode=Mode::Adaptive;s.minimum_samples=8;
  let mut o=Observation::default();o.samples=8;o.body_throughput_samples=3;o.body_bytes_per_second=Some(128.0*1024.0*1024.0);o.last_body_bytes_per_second=o.body_bytes_per_second;
  assert_eq!(contiguous_decision(&s,&o).max_range_bytes,4*1024*1024);
  o.last_body_bytes_per_second=Some(16.0*1024.0*1024.0);assert_eq!(contiguous_decision(&s,&o).max_range_bytes,2*1024*1024);
  o.last_body_bytes_per_second=Some(128.0*1024.0);assert_eq!(contiguous_decision(&s,&o).max_range_bytes,1024*1024);
  o.last_body_bytes_per_second=Some(f64::NAN);assert_eq!(contiguous_decision(&s,&o).max_range_bytes,1024*1024);
  o.last_body_bytes_per_second=o.body_bytes_per_second;
  o.samples=7;assert_eq!(contiguous_decision(&s,&o).max_range_bytes,1024*1024);o.samples=8;
  o.body_throughput_samples=2;assert_eq!(contiguous_decision(&s,&o).max_range_bytes,1024*1024);o.body_throughput_samples=3;
  for mode in [Mode::Observe,Mode::Disabled] {s.mode=mode;assert_eq!(contiguous_decision(&s,&o).max_range_bytes,1024*1024);}
  s.mode=Mode::Adaptive;assert_eq!(contiguous_decision(&s,&o).gap_bytes_added,0);
 }
 use super::*;
 #[test] fn range_cost_uses_current_slow_body_rate_instead_of_stale_fast_average() {
  let mut o=Observation::default();o.body_throughput_samples=3;o.body_bytes_per_second=Some(200e6);o.last_body_bytes_per_second=Some(1e6);o.effective_bytes_per_second=Some(100e6);
  assert_eq!(o.planning_bytes_per_second(),Some(1e6));
  o.last_body_bytes_per_second=Some(400e6);assert_eq!(o.planning_bytes_per_second(),Some(200e6));
  o.last_body_bytes_per_second=Some(f64::NAN);assert_eq!(o.planning_bytes_per_second(),Some(200e6));
  o.body_throughput_samples=0;assert_eq!(o.planning_bytes_per_second(),Some(100e6));
 }
 #[test] fn settings_reject_unbounded_learning_and_overfetch() {let mut s=Settings {index_headers:None,max_source_requests:None,mode:Mode::Adaptive,minimum_samples:16,max_overfetch_ratio:2,serial_transport:Some(SerialTransport::Sdk),refresh_payload_costs:None};assert!(s.validate().is_ok());s.minimum_samples=0;assert!(s.validate().is_err());s.minimum_samples=16;s.max_overfetch_ratio=100;assert!(s.validate().is_err());}
 #[test] fn payload_refresh_option_roundtrips_and_legacy_documents_remain_valid() {
  let legacy=serde_json::json!({"mode":"adaptive","minimumSamples":8,"maxOverfetchRatio":2});
  let mut settings:Settings=serde_json::from_value(legacy).unwrap();assert!(settings.refresh_payload_costs.is_none());
  for value in [true,false] {settings.refresh_payload_costs=Some(value);let saved=serde_json::to_value(&settings).unwrap();assert_eq!(saved["refreshPayloadCosts"],value);let restored:Settings=serde_json::from_value(saved).unwrap();assert_eq!(restored.effective_refresh_payload_costs(),value);}
 }
 #[test] fn transport_choice_roundtrips_and_rejects_unknown_values() {let mut v=serde_json::json!({"mode":"observe","minimumSamples":16,"maxOverfetchRatio":2,"serialTransport":"curl"});let s:Settings=serde_json::from_value(v.clone()).unwrap();assert_eq!(s.effective_serial_transport(),SerialTransport::Curl);assert_eq!(serde_json::to_value(s).unwrap()["serialTransport"],"curl");v["serialTransport"]="typo".into();assert!(serde_json::from_value::<Settings>(v).is_err());}
 #[test] fn legacy_policy_keeps_implicit_transport_but_explicit_sdk_is_persisted() {let v=serde_json::json!({"mode":"observe","minimumSamples":16,"maxOverfetchRatio":2});let mut s:Settings=serde_json::from_value(v.clone()).unwrap();assert!(s.serial_transport.is_none());assert_eq!(serde_json::to_value(&s).unwrap(),v);s.serial_transport=Some(SerialTransport::Sdk);assert_eq!(serde_json::to_value(&s).unwrap()["serialTransport"],"sdk");}
 #[test] fn transport_counters_are_available_without_latency_learning() {let p=Profile::default();p.serial_counts(3,4);let s=p.snapshot();assert_eq!((s.sdk_serial_reads,s.native_serial_requests,s.samples),(3,4,0));}
 #[test] fn bulk_transfers_train_rate_without_inventing_native_header_latency() {
  let p=Profile::default();p.record(1024,Duration::from_millis(2),Duration::from_millis(3));
  p.record_transfer(32768,None,Duration::from_millis(4));
  let s=p.snapshot();assert_eq!(s.samples,2);assert_eq!(s.bytes,33792);
  assert_eq!(s.throughput_samples,1);assert_eq!(s.latency_ms,Some(2.0));
  assert_eq!(s.effective_bytes_per_second,Some(8_192_000.0));
  p.record_transfer(16384,None,Duration::from_millis(1));
  assert_eq!(p.snapshot().throughput_samples,1);
 }
 #[test] fn planner_separates_round_trip_latency_from_measured_body_transfer() {
  let p=Profile::default();
  for _ in 0..2 {p.record(32768,Duration::from_millis(20),Duration::from_micros(20250));}
  let before=p.snapshot();assert_eq!(before.planning_bytes_per_second(),before.effective_bytes_per_second);
  p.record(32768,Duration::from_millis(20),Duration::from_micros(20250));
  let o=p.snapshot();assert_eq!(o.body_throughput_samples,3);
  assert_eq!(o.planning_bytes_per_second(),Some(131072000.0));
  assert_eq!(estimated_ms(1,1048576,1,20.0,o.planning_bytes_per_second().unwrap()),28.0);
  assert!(estimated_ms(1,1048576,1,20.0,o.effective_bytes_per_second.unwrap())>600.0);
 }
 #[test] fn unknown_zero_or_inverted_body_intervals_do_not_train_body_rate() {
  let p=Profile::default();p.record_transfer(32768,None,Duration::from_millis(20));
  p.record(32768,Duration::from_millis(20),Duration::from_millis(20));
  p.record(32768,Duration::from_millis(21),Duration::from_millis(20));
  let o=p.snapshot();assert_eq!(o.body_throughput_samples,0);assert_eq!(o.body_bytes_per_second,None);
  assert_eq!(o.planning_bytes_per_second(),o.effective_bytes_per_second);
 }
 #[tokio::test(flavor="multi_thread",worker_threads=8)] async fn bursts_within_capacity_do_not_enter_the_wait_queue() {
  for _ in 0..8 {
   let p=Arc::new(Profile::default());let limit=settings().effective_source_requests();
   let barrier=Arc::new(tokio::sync::Barrier::new(limit+1));let (ready_tx,mut ready_rx)=tokio::sync::mpsc::channel(limit);
   let (release_tx,release_rx)=tokio::sync::watch::channel(false);let mut tasks=vec![];
   for _ in 0..limit {let p=p.clone();let barrier=barrier.clone();let ready=ready_tx.clone();let mut release=release_rx.clone();
    tasks.push(tokio::spawn(async move {barrier.wait().await;let _permit=p.acquire().await;ready.send(()).await.unwrap();release.wait_for(|v|*v).await.unwrap();}));
   }
   barrier.wait().await;
   for _ in 0..limit {tokio::time::timeout(Duration::from_secs(2),ready_rx.recv()).await.unwrap().unwrap();}
   let snapshot=p.snapshot();assert_eq!(snapshot.active_reads,limit as u64);assert_eq!(snapshot.admission_waits,0,"available slots must not be mistaken for source overload");
   release_tx.send(true).unwrap();for task in tasks {task.await.unwrap();}assert_eq!(p.snapshot().active_reads,0);
  }
 }
 #[test] fn admission_identity_is_shared_above_128_sources() {
  let prefix=uuid::Uuid::new_v4().to_string();let entries=(0..140).map(|n|{let id=format!("{prefix}-{n}");let p=profile(&id);(id,p)}).collect::<Vec<_>>();
  for (id,p) in &entries {assert!(Arc::ptr_eq(p,&profile(id)),"source admission profile was not retained for {id}");}
  for (id,_) in entries {reset(&id);}
 }
 #[tokio::test] async fn source_limit_is_shared_cancellable_and_releases_after_drop() {
  let p=Arc::new(Profile::default());let limit=settings().effective_source_requests();let mut permits=vec![];
  for _ in 0..limit {permits.push(p.acquire().await);}
  assert!(tokio::time::timeout(Duration::from_millis(10),p.acquire()).await.is_err());
  permits.pop();let permit=tokio::time::timeout(Duration::from_millis(100),p.acquire()).await.unwrap();drop(permit);drop(permits);
  assert_eq!(p.1.load(std::sync::atomic::Ordering::Acquire),0);
 }
 #[test] fn source_limit_rejects_zero_and_unbounded_values() {let mut s=Settings::default();s.max_source_requests=Some(0);assert!(s.validate().is_err());s.max_source_requests=Some(65);assert!(s.validate().is_err());s.max_source_requests=Some(4);assert!(s.validate().is_ok());}
 #[test] fn cost_accounts_for_parallel_waves_and_extra_bytes() {assert!(estimated_ms(8,1000,4,10.0,1e6)<estimated_ms(16,1000,4,10.0,1e6));assert!(estimated_ms(8,1000000,4,10.0,1e6)>estimated_ms(16,1000,4,10.0,1e6));}
}

/// An explicit metadata bandwidth allowance, separate from packet overfetch.
#[derive(Clone,Copy,Debug,PartialEq,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub enum IndexHeaderMode {Sparse,Dense,Adaptive}
#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct IndexHeaders {pub mode:IndexHeaderMode,pub window_bytes:u32,pub max_extra_bytes:u64}
impl IndexHeaders {
 pub fn validate(&self)->Result<()> {
  ensure!((1024..=8388608).contains(&self.window_bytes),"indexHeaders.windowBytes must be1024 through8388608");
  ensure!(self.max_extra_bytes>=u64::from(self.window_bytes) && self.max_extra_bytes<=64*1024*1024,"indexHeaders.maxExtraBytes must cover one window and be at most64MiB");Ok(())
 }
}
#[derive(Clone,Debug,serde::Serialize)]
#[serde(rename_all="camelCase")]
pub struct IndexHeaderDecision {pub reason:String,pub window_bytes:u32,pub sample_count:u64,pub throughput_samples:u64,pub estimated_sparse_ms:Option<f64>,pub estimated_window_ms:Option<f64>,pub max_extra_bytes:u64}
pub fn index_header_plan(policy:&IndexHeaders,settings:&Settings,observation:&Observation)->IndexHeaderDecision {
 let mut d=IndexHeaderDecision {reason:"sparseConfigured".into(),window_bytes:0,sample_count:observation.samples,throughput_samples:observation.throughput_samples,estimated_sparse_ms:None,estimated_window_ms:None,max_extra_bytes:policy.max_extra_bytes};
 if policy.mode==IndexHeaderMode::Sparse {return d;}
 if policy.mode==IndexHeaderMode::Dense {d.reason="denseConfigured".into();d.window_bytes=policy.window_bytes;return d;}
 if settings.mode==Mode::Disabled {d.reason="learningDisabled".into();return d;}
 if observation.samples<u64::from(settings.minimum_samples) || observation.throughput_samples<3 {d.reason="insufficientMeasurements".into();return d;}
 let (Some(latency),Some(rate))=(observation.latency_ms,observation.planning_bytes_per_second()) else {d.reason="missingLatencyOrThroughput".into();return d;};
 // Full PLT segments are at most65537 bytes including the marker. Compare
 // serial sparse reads with one window, not unrelated concurrent packet IO.
 let markers=(u64::from(policy.window_bytes)/65537).max(1) as usize;
 let sparse=estimated_ms(markers,markers as u64*1024,1,latency,rate);
 let dense=estimated_ms(1,u64::from(policy.window_bytes),1,latency,rate);
 d.estimated_sparse_ms=Some(sparse);d.estimated_window_ms=Some(dense);
 if dense<sparse*0.75 {if settings.mode==Mode::Adaptive {d.reason="predictedHeaderRoundTripSavings".into();d.window_bytes=policy.window_bytes;}else {d.reason="headerSavingsObserved".into();}}else {d.reason="headerBytesCostMore".into();}d
}
impl Profile {pub fn index_header_decision(&self,d:IndexHeaderDecision) {self.0.lock().unwrap_or_else(|e|e.into_inner()).index_header_decision=Some(d);}}
#[cfg(test)] mod index_header_tests {
 use super::*;
 #[test] fn distant_fast_source_can_batch_headers_without_double_charging_rtt() {
  let profile=Profile::default();for _ in 0..16 {profile.record(32768,Duration::from_millis(20),Duration::from_micros(20250));}
  let p=IndexHeaders {mode:IndexHeaderMode::Adaptive,window_bytes:1048576,max_extra_bytes:32*1024*1024};
  let mut s=Settings::default();s.mode=Mode::Adaptive;
  let o=profile.snapshot();let plan=index_header_plan(&p,&s,&o);assert_eq!(plan.window_bytes,1048576);
  let mut legacy=o;legacy.body_throughput_samples=0;
  assert_eq!(index_header_plan(&p,&s,&legacy).window_bytes,0);
 }
 #[test] fn adaptive_headers_require_measurements_and_reject_slow_bandwidth() {
  let p=IndexHeaders {mode:IndexHeaderMode::Adaptive,window_bytes:1048576,max_extra_bytes:32*1024*1024};let mut s=Settings::default();s.mode=Mode::Adaptive;let mut o=Observation::default();assert_eq!(index_header_plan(&p,&s,&o).window_bytes,0);
  o.samples=32;o.throughput_samples=3;o.latency_ms=Some(1.0);o.effective_bytes_per_second=Some(20_000_000.0);assert_eq!(index_header_plan(&p,&s,&o).window_bytes,0);
  o.latency_ms=Some(20.0);assert_eq!(index_header_plan(&p,&s,&o).window_bytes,1048576);s.mode=Mode::Observe;assert_eq!(index_header_plan(&p,&s,&o).window_bytes,0);s.mode=Mode::Adaptive;
  o.effective_bytes_per_second=Some(1_000_000.0);assert_eq!(index_header_plan(&p,&s,&o).window_bytes,0);
 }
 #[test] fn index_header_policy_roundtrips_and_bounds_bandwidth() {
  let mut s:Settings=serde_json::from_value(serde_json::json!({"mode":"adaptive","minimumSamples":8,"maxOverfetchRatio":2,"indexHeaders":{"mode":"adaptive","windowBytes":1048576,"maxExtraBytes":33554432}})).unwrap();assert!(s.validate().is_ok());assert!(serde_json::to_value(&s).unwrap().get("indexHeaders").is_some());s.index_headers.as_mut().unwrap().max_extra_bytes=100;assert!(s.validate().is_err());
 }
}
