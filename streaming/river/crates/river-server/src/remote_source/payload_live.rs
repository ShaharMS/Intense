//! Trial: revise only unstarted overfetch after required body reads reveal a drop.
use super::*;
use std::collections::VecDeque;
pub(super) fn required_only(range:&Range<u64>,required:&[Range<u64>])->bool {
 let mut cursor=range.start;
 for r in required.iter().filter(|r|r.end>range.start && r.start<range.end) {
  if r.start>cursor {return false;}
  cursor=cursor.max(r.end.min(range.end));if cursor==range.end {return true;}
 }
 false
}
pub(super) fn intersect_pending(required:&[Range<u64>],pending:&[Range<u64>])->Vec<Range<u64>> {
 let mut coverage=pending.to_vec();coverage.sort_by_key(|r|r.start);
 let (mut i,mut j)=(0,0);let mut result=vec![];
 while i<required.len() && j<coverage.len() {
  let (a,b)=(&required[i],&coverage[j]);let start=a.start.max(b.start);let end=a.end.min(b.end);
  if start<end {result.push(start..end);}
  if a.end<=b.end {i+=1;}else{j+=1;}
 }
 result
}
fn replacement_fits(exact_count:usize,exact_bytes:u64,old_bytes:u64,quota:u64,remaining:u64,new_cost:f64,old_cost:f64)->bool {
 exact_count>0 && exact_bytes<old_bytes && exact_bytes<=remaining && exact_count as u64<=quota && new_cost<old_cost*0.95
}
impl RemoteSource {
 pub(super) fn fetch_live_payload(&self,required:Vec<Range<u64>>,mut ranges:Vec<Range<u64>>,mut leases:HashMap<u64,Arc<[u8]>>,context:&mut ReadContext,lanes:usize,deadline:Instant,cancel:Option<watch::Receiver<bool>>)->Result<HashMap<u64,Arc<[u8]>>> {
  ranges.sort_by_key(|r|(!(r.end-r.start>=crate::read_policy::THROUGHPUT_SAMPLE_BYTES && required_only(r,&required)),!required_only(r,&required),r.start));
  let client=self.range_client(context);
  let mut checked_rate=self.read_profile.snapshot().planning_bytes_per_second().unwrap_or(f64::MAX);
  self.runtime.block_on(async {
   let mut pending=VecDeque::from(ranges);let mut tasks=tokio::task::JoinSet::new();let mut inflight_bytes=0u64;
   let spawn=|tasks:&mut tokio::task::JoinSet<Result<(Range<u64>,Vec<u8>)>>,range:Range<u64>| {
    let client=client.clone();let cancel=cancel.clone();
    tasks.spawn(async move {let data=client.fetch(range.clone(),deadline,cancel).await?;Ok((range,data))}.in_current_span());
   };
   while tasks.len()<lanes {if let Some(range)=pending.pop_front(){inflight_bytes+=range.end-range.start;spawn(&mut tasks,range);}else{break;}}
   while let Some(result)=tasks.join_next().await {
    let (range,data)=result.context("Live payload range worker failed")??;context.check()?;
    inflight_bytes=inflight_bytes.saturating_sub(range.end-range.start);
    context.stats.bytes+=data.len() as u64;context.stats.requests+=1;
    let blocks=self.cache.put(&self.cache_key,self.block_size,range.start,&data);
    if self.cache.limit<8*1024*1024 {context.recent.remember(self.block_size,&blocks);}leases.extend(blocks);
    let observation=self.read_profile.snapshot();
    let settings=crate::read_policy::settings();
    if settings.mode==crate::read_policy::Mode::Adaptive && observation.samples>=u64::from(settings.minimum_samples) {
     if let Some(rate)=observation.planning_bytes_per_second().filter(|r|*r<checked_rate*0.5) {
      checked_rate=rate;
      let old=pending.iter().cloned().collect::<Vec<_>>();let exact=intersect_pending(&required,&old);
      let old_bytes=old.iter().map(|r|r.end-r.start).sum::<u64>();let exact_bytes=exact.iter().map(|r|r.end-r.start).sum::<u64>();
      // Reserve every outstanding task even if it has not yet claimed its
      // wire allowance. Double-counting claimed tasks is conservative.
      let quota=context.requests_limit.saturating_sub(context.wire_budget.used()).saturating_sub(tasks.len() as u64);
      let remaining=context.limit.saturating_sub(context.stats.bytes).saturating_sub(inflight_bytes);
      let latency=observation.latency_ms.unwrap_or(0.0);
      let old_cost=crate::read_policy::estimated_ms(old.len(),old_bytes,lanes,latency,rate);
      let new_cost=crate::read_policy::estimated_ms(exact.len(),exact_bytes,lanes,latency,rate);
      if replacement_fits(exact.len(),exact_bytes,old_bytes,quota,remaining,new_cost,old_cost) {
       self.read_profile.decision(crate::read_policy::Decision {reason:"currentBodyRateAvoidsUnstartedOverfetch".into(),sample_count_at_decision:observation.samples,throughput_samples_at_decision:observation.throughput_samples,original_requests:old.len(),planned_requests:exact.len(),original_bytes:old_bytes,planned_bytes:exact_bytes,estimated_original_ms:old_cost,estimated_planned_ms:new_cost,applied:true});
       tracing::info!(context=context.trace_id,old_ranges=old.len(),new_ranges=exact.len(),bytes_avoided=old_bytes-exact_bytes,"Replanned unstarted payload after source slowdown");
       pending=VecDeque::from(exact);
      }
     }
    }
    while tasks.len()<lanes {if let Some(range)=pending.pop_front(){inflight_bytes+=range.end-range.start;spawn(&mut tasks,range);}else{break;}}
   }
   Ok::<_,anyhow::Error>(())
  })?;
  Ok(leases)
 }
}
#[cfg(test)]mod tests {
 use super::*;
 #[test]fn intersections_stay_inside_unstarted_coverage_and_preserve_gaps() {
  let required=vec![0..10,20..30,40..50,60..70];
  assert_eq!(intersect_pending(&required,&[35..65,5..25]),vec![5..10,20..25,40..50,60..65]);
  assert!(required_only(&(0..10),&required));assert!(!required_only(&(0..30),&required));
  assert_eq!(intersect_pending(&required,&[20..30]),vec![20..30]);
 }
 #[test]fn lower_byte_cost_cannot_override_attempt_or_memory_caps() {
  assert!(replacement_fits(21,800,2400,100,1000,3500.0,9900.0));
  assert!(!replacement_fits(21,800,2400,4,1000,3500.0,9900.0));
  assert!(!replacement_fits(21,800,2400,100,799,3500.0,9900.0));
  assert!(!replacement_fits(0,0,2400,100,1000,0.0,9900.0));
  assert!(!replacement_fits(21,800,2400,100,1000,9800.0,9900.0));
 }
}
