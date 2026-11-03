//! Share extra OpenJPEG CPU workers across admitted decode jobs.
use std::sync::{Arc,OnceLock,atomic::{AtomicUsize,Ordering}};
static ACTIVE_WORK:AtomicUsize=AtomicUsize::new(0);
#[derive(Debug)]
pub(crate) struct WorkTicket<'a>(&'a AtomicUsize);
impl<'a> WorkTicket<'a> {
 fn enter(counter:&'a AtomicUsize)->Arc<Self> {counter.fetch_add(1,Ordering::AcqRel);Arc::new(Self(counter))}
}
impl Drop for WorkTicket<'_> {fn drop(&mut self){self.0.fetch_sub(1,Ordering::AcqRel);}}
pub(crate) fn request_ticket()->Arc<WorkTicket<'static>> {WorkTicket::enter(&ACTIVE_WORK)}
struct Pool { free:AtomicUsize,cpus:usize,slots:usize }
impl Pool {
 fn new(cpus:usize,slots:usize)->Self {Self {free:AtomicUsize::new(cpus.saturating_sub(slots)),cpus,slots}}
 fn acquire(&self,maximum:usize,large:bool)->Budget<'_> {
  let wanted=if large {maximum.saturating_sub(1)}else{0};
  let mut free=self.free.load(Ordering::Relaxed);
  let claimed=loop {let take=wanted.min(free);match self.free.compare_exchange_weak(free,free-take,Ordering::AcqRel,Ordering::Relaxed) {Ok(_)=>break take,Err(now)=>free=now}};
  Budget {pool:self,claimed}
 }
}
pub(crate) struct Budget<'a> {pool:&'a Pool,claimed:usize}
impl Budget<'_> {pub(crate) fn threads(&self)->usize {1+self.claimed}}
impl Drop for Budget<'_> {fn drop(&mut self) {self.pool.free.fetch_add(self.claimed,Ordering::Release);}}
fn automatic_max_threads(cpus:usize)->usize {match cpus {n if n>=16=>16,n if n>=8=>4,n if n>=4=>2,_=>1}}
pub(crate) fn default_max_threads()->usize {
 if std::env::var("RIVER_PROFILE").is_ok_and(|p|p=="compact" || p=="throughput16") {return 1;}
 automatic_max_threads(std::thread::available_parallelism().map_or(1,usize::from))
}
fn shared_pool()->&'static Pool {
 static POOL:OnceLock<Pool>=OnceLock::new();
 POOL.get_or_init(||Pool::new(std::thread::available_parallelism().map_or(1,usize::from),crate::decode_concurrency_from_env()))
}
pub(crate) fn acquire(maximum:usize,large:bool)->Budget<'static> {
 shared_pool().acquire(maximum,large)
}

fn useful_pixels(request:&crate::engine::TileDecodeRequest)->u64 {
 let scale=1u32.checked_shl(u32::from(request.reduction)).unwrap_or(u32::MAX);
 request.tiles.iter().map(|&(x,y)| {
  let (x0,x1)=super::clamped_tile_bounds(x,request.source_tile,request.source.width);
  let (y0,y1)=super::clamped_tile_bounds(y,request.source_tile,request.source.height);
  u64::from((x1-x0).div_ceil(scale))*u64::from((y1-y0).div_ceil(scale))
 }).sum()
}
fn fair_maximum(maximum:usize,active:usize,cpus:usize,slots:usize)->usize {
 let runnable=active.max(1).min(slots.max(1));
 maximum.min((cpus.max(1)/runnable).max(1))
}
fn sized_maximum(maximum:usize,pixels:u64,active:usize,cpus:usize,slots:usize)->usize {
 if pixels<512*1024 {return maximum.min(4);}
 fair_maximum(maximum,active,cpus,slots)
}
fn full_view_native_groups(width:u32,height:u32,source_tile:u32,native_width:u32,native_height:u32)->u64 {
 if width==0 || height==0 || native_width==0 || native_height==0 || source_tile<width || source_tile<height {return 0;}
 // Groups contain at most four adjacent native tiles and never cross rows.
 // A smaller configured batch only increases the available independent work.
 u64::from(width.div_ceil(native_width).div_ceil(4))*u64::from(height.div_ceil(native_height))
}
fn compact_native_groups(request:&crate::engine::TileDecodeRequest)->u64 {
 if request.tiles.as_slice()!=[(0,0)] || request.source.reference_origin!=(0,0) || !super::reduced_tiles::eligible(&request.source) {return 0;}
 if request.source.technical_reduction(request.reduction).ok()!=Some(0)
  || std::env::var("RIVER_S3_NATIVE_TILES").is_ok_and(|v|v=="1" || v=="true") {return 0;}
 let Some(layout)=request.source.native_tile_layout else{return 0;};
 if layout.origin_x!=0 || layout.origin_y!=0 {return 0;}
 full_view_native_groups(request.source.width,request.source.height,request.source_tile,layout.width,layout.height)
}
fn maximum_with_groups(maximum:usize,pixels:u64,active:usize,cpus:usize,slots:usize,groups:u64)->usize {
 let normal=sized_maximum(maximum,pixels,active,cpus,slots);
 if groups<128 {return normal;}
 // Amortize worker startup across at least32independent groups per worker.
 let tile_maximum=maximum.min((groups/32).min(usize::MAX as u64) as usize);
 normal.max(fair_maximum(tile_maximum,active,cpus,slots))
}

pub(crate) fn request_maximum(request:&crate::engine::TileDecodeRequest)->usize {
 let pool=shared_pool();
 maximum_with_groups(request.decoder_threads,useful_pixels(request),ACTIVE_WORK.load(Ordering::Acquire),pool.cpus,pool.slots,compact_native_groups(request))
}

pub(crate) fn large_request(request:&crate::engine::TileDecodeRequest)->bool {
 if compact_native_groups(request)>=128 {return true;}
 let scale=1u32.checked_shl(u32::from(request.reduction)).unwrap_or(u32::MAX);
 let useful=useful_pixels(request);
 let native=request.source.native_tile_layout.map_or(useful,|h|u64::from(h.width.div_ceil(scale))*u64::from(h.height.div_ceil(scale)));
 useful>=512*256 && (native>=512*256 || super::reduced_tiles::eligible(&request.source))
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn active_jobs_share_cpu_capacity_and_release_all_leases() {
  for maximum in [1,4,8,16] {for cpus in 1..=32 {for slots in 1..=cpus {
   let pool=Pool::new(cpus,slots);let jobs=(0..slots).map(|_|pool.acquire(maximum,true)).collect::<Vec<_>>();
   assert!(jobs.iter().map(Budget::threads).sum::<usize>()<=cpus);
   drop(jobs);assert_eq!(pool.free.load(Ordering::Relaxed),cpus-slots);
  }}}
 }
 #[test] fn automatic_caps_scale_without_spending_small_machine_cpus() {
  for (cpus,expected) in [(1,1),(3,1),(4,2),(7,2),(8,4),(15,4),(16,16),(22,16),(64,16)] {
   assert_eq!(automatic_max_threads(cpus),expected);
  }
 }
 #[test] fn preview_workers_do_not_expand_moderate_tile_budgets() {
  assert_eq!(sized_maximum(8,512*512,1,8,4),4);
  assert_eq!(sized_maximum(8,512*1024-1,1,8,4),4);
  assert_eq!(sized_maximum(8,512*1024,1,8,4),8);
  assert_eq!(sized_maximum(4,1024*2560,1,16,1),4);
  assert_eq!(sized_maximum(16,1024*2560,1,16,1),16);
  assert_eq!(sized_maximum(1,1024*2560,1,16,1),1);
 }
 #[test] fn queued_jobs_share_only_runnable_decode_slots() {
  for active in [2,3,8,16] {assert_eq!(sized_maximum(16,1024*2560,active,16,1),16);}
  for (active,slots,expected) in [(2,8,8),(3,8,5),(4,8,4),(8,8,2),(16,8,2),(8,4,4)] {
   assert_eq!(sized_maximum(16,1024*2560,active,16,slots),expected);
  }
  assert_eq!(sized_maximum(16,1024*2560,16,4,8),1);
  assert_eq!(sized_maximum(1,1024*2560,8,16,1),1);
 }
 #[test] fn work_ticket_clones_count_one_job_and_unwind_releases_activity() {
  let counter=AtomicUsize::new(0);let first=WorkTicket::enter(&counter);let clone=first.clone();
  assert_eq!(counter.load(Ordering::Acquire),1);
  let second=WorkTicket::enter(&counter);drop(first);assert_eq!(counter.load(Ordering::Acquire),2);
  drop(clone);assert_eq!(counter.load(Ordering::Acquire),1);
  let _=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{let _work=WorkTicket::enter(&counter);panic!("cancelled render");}));
  assert_eq!(counter.load(Ordering::Acquire),1);drop(second);assert_eq!(counter.load(Ordering::Acquire),0);
 }
 #[test] fn tiny_work_does_not_consume_spare_cpu_or_ignore_explicit_one() {
  let pool=Pool::new(16,8);assert_eq!(pool.acquire(4,false).threads(),1);assert_eq!(pool.acquire(1,true).threads(),1);assert_eq!(pool.free.load(Ordering::Relaxed),8);
 }
 #[test] fn simultaneous_claims_cannot_oversubscribe_spare_workers() {
  let pool=Pool::new(16,8);let gate=std::sync::Barrier::new(8);std::thread::scope(|scope| {
   let jobs=(0..8).map(|_|scope.spawn(||{let b=pool.acquire(8,true);gate.wait();b})).collect::<Vec<_>>();
   let jobs=jobs.into_iter().map(|j|j.join().unwrap()).collect::<Vec<_>>();assert!(jobs.iter().map(Budget::threads).sum::<usize>()<=16);
  });assert_eq!(pool.free.load(Ordering::Relaxed),8);
 }
 #[test] fn tiny_whole_image_outputs_account_for_native_tile_work() {
  let groups=full_view_native_groups(65536,65536,131072,1024,1024);
  assert_eq!(groups,1024);
  assert_eq!(maximum_with_groups(16,256*256,1,16,2,groups),16);
  assert_eq!(maximum_with_groups(16,256*256,2,16,2,groups),8);
  assert_eq!(maximum_with_groups(4,256*256,1,4,1,groups),4);
  assert_eq!(maximum_with_groups(1,256*256,1,16,2,groups),1);
  assert_eq!(maximum_with_groups(16,256*256,1,16,2,64),4);
  assert_eq!(full_view_native_groups(65536,65536,32768,1024,1024),0);
  assert_eq!(full_view_native_groups(493,235,512,32,32),32);
  assert_eq!(full_view_native_groups(1,1,512,0,1),0);
 }

}
