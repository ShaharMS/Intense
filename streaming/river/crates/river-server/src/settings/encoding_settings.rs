//! Persisted encoding configuration with per-render snapshots.
use std::{cell::Cell, sync::{Mutex,OnceLock,atomic::{AtomicU8,Ordering}}};
use anyhow::{Result,ensure};
#[derive(Clone,Copy,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Settings { pub webp_method:u8 }
static VALUE:OnceLock<AtomicU8>=OnceLock::new();
static WRITER:Mutex<()>=Mutex::new(());
thread_local! { static RENDER:Cell<Option<u8>>=const {Cell::new(None)}; }
fn value()->&'static AtomicU8 { VALUE.get_or_init(||AtomicU8::new(std::env::var("RIVER_WEBP_METHOD").ok().and_then(|v|v.parse::<i32>().ok()).unwrap_or(4).clamp(0,6) as u8)) }
pub fn snapshot()->u8 { value().load(Ordering::Acquire) }
pub fn method()->u8 { RENDER.with(|slot|slot.get()).unwrap_or_else(snapshot) }
pub struct Scope(Option<u8>);
impl Scope { pub fn new(method:u8)->Self {Self(RENDER.with(|slot|slot.replace(Some(method))))} }
impl Drop for Scope {fn drop(&mut self) { RENDER.with(|slot|slot.set(self.0)); } }
pub fn restore()->Result<()> {let d=crate::configuration::snapshot()?;value().store(d.encoding.webp_method,Ordering::Release);crate::read_policy::apply(d.read_policy);Ok(())}
pub fn apply(settings:Settings) {value().store(settings.webp_method,Ordering::Release);}
pub fn update(settings:Settings)->Result<()> {
    ensure!(settings.webp_method<=6,"WebP method must be 0 through 6");
    let _guard=WRITER.lock().unwrap_or_else(|e|e.into_inner());
    crate::configuration::update(|d|{d.encoding=settings;Ok(())})?;apply(settings);Ok(())
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn render_snapshot_is_thread_local_nested_and_released() {
        let default=snapshot();{let _one=Scope::new(1);assert_eq!(method(),1);{let _two=Scope::new(6);assert_eq!(method(),6);}
            assert_eq!(method(),1);assert_eq!(std::thread::spawn(method).join().unwrap(),default);}
        assert_eq!(method(),default);
    }
}
