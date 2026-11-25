//! Bounded, always-on byte accounting; never retains payloads or object keys.
//! Needed bytes mean consumer-requested ranges before block alignment/gap filling,
//! not a claim about the theoretical minimum entropy needed by a decoder.
use std::{ops::Range,sync::{Arc,Mutex,OnceLock}};
const MAX_INTERVALS:usize=8192;
#[derive(Default)] struct State { needed:Vec<Range<u64>>, fetched:Vec<(Range<u64>,u64)>, bytes:u64, failed:bool, payload:bool, overflow:bool, finished:bool }
#[derive(Default)] pub(super) struct Ledger { profile:OnceLock<Arc<crate::read_policy::Profile>>, budget:OnceLock<Arc<super::wire_budget::Budget>>, state:Mutex<State> }
fn merged(mut spans:Vec<Range<u64>>)->Vec<Range<u64>> {
 spans.sort_unstable_by_key(|r|(r.start,r.end));let mut out:Vec<Range<u64>>=Vec::new();
 for r in spans {if r.start>=r.end {continue;}if let Some(last)=out.last_mut().filter(|v|v.end>=r.start){last.end=last.end.max(r.end);}else{out.push(r);}}
 out
}
fn intersection(a:&[Range<u64>],b:&[Range<u64>])->u64 {
 let(mut i,mut j,mut total)=(0,0,0u64);
 while i<a.len()&&j<b.len(){total=total.saturating_add(a[i].end.min(b[j].end).saturating_sub(a[i].start.max(b[j].start)));if a[i].end<=b[j].end{i+=1;}else{j+=1;}}
 total
}
impl Ledger {
 pub fn bind(&self,p:Arc<crate::read_policy::Profile>){let _=self.profile.set(p);}
 pub fn bind_budget(&self,b:Arc<super::wire_budget::Budget>){let _=self.budget.set(b);}
 pub fn need(&self,r:Range<u64>){let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());if s.finished{return;}if s.needed.len()>=MAX_INTERVALS{s.overflow=true;}else{s.needed.push(r);}}
 pub fn fetched(&self,r:Range<u64>,bytes:u64,work_ns:u64){let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());if s.finished{return;}s.bytes=s.bytes.saturating_add(bytes);if s.fetched.len()>=MAX_INTERVALS{s.overflow=true;}else{s.fetched.push((r,work_ns));}}
 pub fn payload(&self){self.state.lock().unwrap_or_else(|e|e.into_inner()).payload=true;}
 pub fn cost(&self,range:Range<u64>)->u64{self.state.lock().unwrap_or_else(|e|e.into_inner()).fetched.iter().rev().find(|(r,_)|*r==range).map_or(0,|(_,ns)|*ns)}
 pub fn failed(&self){self.state.lock().unwrap_or_else(|e|e.into_inner()).failed=true;}
 pub fn finish(&self,cancelled:bool){
  let Some(profile)=self.profile.get() else{return;};
  let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());if s.finished{return;}s.finished=true;
  if s.bytes==0&&!s.failed&&!s.overflow&&!cancelled{return;}
  // A retry can consume an unobserved partial body before eventually succeeding.
  // Compare physical attempts across this whole preparation, including parallel reads.
  let unmatched_attempts=self.budget.get().is_some_and(|budget|budget.used()>s.fetched.len() as u64);
  let needed=merged(std::mem::take(&mut s.needed));let fetched=merged(std::mem::take(&mut s.fetched).into_iter().map(|(r,_)|r).collect());
  let useful=intersection(&needed,&fetched);
  profile.record_overfetch(s.payload,s.bytes,useful,!s.failed&&!s.overflow&&!cancelled&&!unmatched_attempts&&useful>0,
   if s.overflow{"interval accounting limit exceeded"}else if cancelled{"cancelled request"}else if s.failed{"failed or partial source read"}else if unmatched_attempts{"retry or unaccounted physical source attempt; partial response bytes are unknown"}else if useful==0{"no attributable demanded origin bytes"}else{"measured successful origin bytes and unique demanded intersection"});
 }
}
#[cfg(test)] mod tests {
 use super::*;
 #[test]fn overlapping_demands_and_repeated_fetches_are_not_hidden(){
  let needed=merged(vec![10..30,20..40,100..120]);let fetched=merged(vec![0..64,0..64]);assert_eq!(intersection(&needed,&fetched),30);
  // Wire bytes remain 128; denominator is 30, not 60 or cached-only demand.
 }
 #[test]fn successful_retry_does_not_disguise_unknown_partial_bytes(){
  let l=Ledger::default();let profile=Arc::new(crate::read_policy::Profile::default());l.bind(profile.clone());
  let budget=super::super::wire_budget::Budget::new(3);budget.claim().unwrap();budget.claim().unwrap();l.bind_budget(budget);
  l.need(0..10);l.fetched(0..100,100,1);l.finish(false);
  let observed=profile.snapshot().metadata_overfetch;assert_eq!(observed.measured_samples,0);assert_eq!(observed.unmeasured_samples,1);assert!(observed.reason.contains("retry"));
 }
 #[test]fn aborted_transfer_marks_sample_unreliable(){let ledger=Arc::new(Ledger::default());{let _guard=TransferGuard::new(ledger.clone());}assert!(ledger.state.lock().unwrap().failed);}
 #[test]fn completed_transfer_keeps_sample_eligible(){let ledger=Arc::new(Ledger::default());{let mut guard=TransferGuard::new(ledger.clone());guard.complete();}assert!(!ledger.state.lock().unwrap().failed);}
 #[test]fn bounded_ledger_does_not_train_on_incomplete_accounting(){let l=Ledger::default();for _ in 0..=MAX_INTERVALS{l.need(0..1);}let s=l.state.lock().unwrap();assert!(s.overflow);assert_eq!(s.needed.len(),MAX_INTERVALS);}
}

impl Drop for Ledger {fn drop(&mut self){self.finish(false);}}
pub(super) struct TransferGuard {ledger:Arc<Ledger>,done:bool}
impl TransferGuard {pub fn new(ledger:Arc<Ledger>)->Self{Self{ledger,done:false}}pub fn complete(&mut self){self.done=true;}}
impl Drop for TransferGuard {fn drop(&mut self){if !self.done{self.ledger.failed();}}}
