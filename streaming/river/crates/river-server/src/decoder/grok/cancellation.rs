use std::cell::RefCell;
use tokio::sync::watch;
thread_local! {static CURRENT:RefCell<Option<watch::Receiver<bool>>>=const {RefCell::new(None)};}
pub(crate) struct Scope(Option<watch::Receiver<bool>>);
pub(crate) fn enter(cancel:Option<watch::Receiver<bool>>)->Scope {Scope(CURRENT.with(|slot|slot.replace(cancel)))}
pub(super) fn cancelled()->bool {CURRENT.with(|slot|slot.borrow().as_ref().is_some_and(|cancel|*cancel.borrow()))}
impl Drop for Scope {fn drop(&mut self){CURRENT.with(|slot|slot.replace(self.0.take()));}}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn nested_requests_restore_their_own_cancellation_state(){
  let (send,receive)=watch::channel(false);let outer=enter(Some(receive));assert!(!cancelled());send.send(true).unwrap();assert!(cancelled());
  {let _inner=enter(None);assert!(!cancelled());}assert!(cancelled());drop(outer);assert!(!cancelled());
 }
 #[test] fn failed_decode_does_not_cancel_the_next_request_on_a_reused_thread(){
  let (_,receive)=watch::channel(true);let failed=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{let _scope=enter(Some(receive));assert!(cancelled());panic!("decode failed");}));
  assert!(failed.is_err());assert!(!cancelled());let (_,receive)=watch::channel(false);let _next=enter(Some(receive));assert!(!cancelled());
 }
}
