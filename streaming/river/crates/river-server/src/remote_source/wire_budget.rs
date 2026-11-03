//! Enforce physical source-attempt limits independently of optional diagnostics.
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
tokio::task_local! {static CURRENT:Arc<Budget>;}
#[derive(Debug)]pub struct Budget {used:AtomicU64,limit:AtomicU64}
impl Budget {
 pub fn new(limit:u64)->Arc<Self> {Arc::new(Self {used:AtomicU64::new(0),limit:AtomicU64::new(limit)})}
 pub fn set_limit(&self,limit:u64) {self.limit.store(limit,Ordering::Relaxed);}
 pub fn used(&self)->u64 {self.used.load(Ordering::Relaxed)}
 pub fn claim(&self)->Result<(),std::io::Error> {
  self.used.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|n|(n<self.limit.load(Ordering::Relaxed)).then(||n+1))
   .map(|_|()).map_err(|used|std::io::Error::other(format!("S3 physical request attempt limit exceeded before send (used={used}, limit={})",self.limit.load(Ordering::Relaxed))))
 }
 pub async fn scope<F:std::future::Future>(self:&Arc<Self>,f:F)->F::Output {CURRENT.scope(self.clone(),f).await}
}
pub fn before_send()->Result<(),std::io::Error> {CURRENT.try_with(|b|b.claim()).unwrap_or(Ok(()))}
#[cfg(test)]mod tests {
 use super::*;
 #[test]fn parallel_claims_never_exceed_shared_limit() {
  let b=Budget::new(7);let passed=AtomicU64::new(0);
  std::thread::scope(|s|{for _ in 0..32 {let b=&b;let passed=&passed;s.spawn(move||{if b.claim().is_ok(){passed.fetch_add(1,Ordering::Relaxed);}});}});
  assert_eq!(passed.load(Ordering::Relaxed),7);b.set_limit(8);assert!(b.claim().is_ok());assert!(b.claim().is_err());
 }
 #[tokio::test]async fn nested_scopes_restore_outer_budget() {
  let outer=Budget::new(1);let inner=Budget::new(1);
  outer.scope(async {assert!(before_send().is_ok());inner.scope(async {assert!(before_send().is_ok());assert!(before_send().is_err());}).await;assert!(before_send().is_err());}).await;
  assert!(before_send().is_ok());
 }
 #[test]fn exhausted_budget_reports_usage_without_claiming_another_send() {
  let b=Budget::new(2);b.claim().unwrap();b.claim().unwrap();
  let error=b.claim().unwrap_err().to_string();
  assert!(error.contains("used=2, limit=2"));assert_eq!(b.used(),2);
 }
}
