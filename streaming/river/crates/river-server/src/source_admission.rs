//! Keep serial index waiters outside global I/O admission. The worker releases
//! index-only admission before fetching independent payloads; prefix readers
//! retain their existing whole-preparation admission policy.
use std::{cell::RefCell,marker::PhantomData,rc::Rc,sync::atomic::{AtomicUsize,Ordering}};
use tokio::sync::OwnedMutexGuard;
static ACTIVE:AtomicUsize=AtomicUsize::new(0);
struct Activity<'a>(&'a AtomicUsize);
impl<'a> Activity<'a> {fn enter(counter:&'a AtomicUsize)->Self {counter.fetch_add(1,Ordering::Relaxed);Self(counter)}}
impl Drop for Activity<'_> {fn drop(&mut self) {self.0.fetch_sub(1,Ordering::Relaxed);}}
pub fn active_preparations()->usize {ACTIVE.load(Ordering::Relaxed)}
type Guard=OwnedMutexGuard<()>;
thread_local! {static INDEX:RefCell<Option<Guard>>=const {RefCell::new(None)};}

pub struct Scope {_activity:Activity<'static>,previous:Option<Guard>,_whole:Option<Guard>,_thread:PhantomData<Rc<()>>}
impl Scope {
    pub fn enter(guard:Option<Guard>,index_only:bool)->Self {
        let (whole,index)=if index_only {(None,guard)} else {(guard,None)};
        let previous=INDEX.with(|slot|slot.replace(index));
        Self {_activity:Activity::enter(&ACTIVE),previous,_whole:whole,_thread:PhantomData}
    }
}
impl Drop for Scope {
    fn drop(&mut self) {INDEX.with(|slot|drop(slot.replace(self.previous.take())));}
}
pub fn index_finished() {INDEX.with(|slot|drop(slot.borrow_mut().take()));}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    #[test]
    fn preparation_activity_is_released_after_nested_work_and_unwind() {
        let counter=AtomicUsize::new(0);
        let failed=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _outer=Activity::enter(&counter);assert_eq!(counter.load(Ordering::Relaxed),1);
            {let _inner=Activity::enter(&counter);assert_eq!(counter.load(Ordering::Relaxed),2);}
            assert_eq!(counter.load(Ordering::Relaxed),1);panic!("cancelled preparation");
        }));
        assert!(failed.is_err());assert_eq!(counter.load(Ordering::Relaxed),0);
    }
    #[test]
    fn index_completion_releases_only_index_admission() {
        let gate=Arc::new(Mutex::new(()));
        let scope=Scope::enter(Some(gate.clone().try_lock_owned().unwrap()),true);
        assert!(gate.try_lock().is_err());index_finished();assert!(gate.try_lock().is_ok());drop(scope);
        let scope=Scope::enter(Some(gate.clone().try_lock_owned().unwrap()),false);
        index_finished();assert!(gate.try_lock().is_err());drop(scope);assert!(gate.try_lock().is_ok());
    }
    #[test]
    fn nested_worker_operations_preserve_outer_admission() {
        let outer=Arc::new(Mutex::new(()));let inner=Arc::new(Mutex::new(()));
        let a=Scope::enter(Some(outer.clone().try_lock_owned().unwrap()),true);
        let b=Scope::enter(Some(inner.clone().try_lock_owned().unwrap()),true);
        index_finished();assert!(inner.try_lock().is_ok());assert!(outer.try_lock().is_err());
        drop(b);index_finished();assert!(outer.try_lock().is_ok());drop(a);
    }
    #[test]
    fn failed_worker_unwinds_without_leaking_admission_into_next_request() {
        let gate=Arc::new(Mutex::new(()));
        let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope=Scope::enter(Some(gate.clone().try_lock_owned().unwrap()),true);
            panic!("simulated failed preparation");
        }));
        assert!(result.is_err());assert!(gate.try_lock().is_ok());
        let scope=Scope::enter(Some(gate.clone().try_lock_owned().unwrap()),true);
        index_finished();assert!(gate.try_lock().is_ok());drop(scope);
    }
}
