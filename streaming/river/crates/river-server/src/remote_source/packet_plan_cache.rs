//! Immutable packet geometry only: no payload bytes or source offsets are shared.
use super::{Header,Packet,packet_order};
use std::{collections::{HashMap,BTreeMap},sync::{Arc,Mutex,OnceLock}};
pub(super) type Plan=(Arc<Vec<Packet>>,usize);
#[derive(Clone,Hash,Eq,PartialEq)]
struct Key {bounds:(u32,u32,u32,u32),components:u8,levels:u8,layers:u16,progression:u8,cod:Vec<u8>,prefix:Option<u8>}
impl Key {
 fn new(h:&Header,tile:u16,prefix:Option<u8>)->Self {
  Self {bounds:h.tile_bounds(tile),components:h.components,levels:h.levels,layers:h.layers,
   progression:h.progression,cod:h.cod.clone(),prefix}
 }
}
struct Entry {plan:Plan,bytes:usize,touched:u64}
#[derive(Default)]struct State {entries:HashMap<Key,Entry>,lru:BTreeMap<u64,Key>,bytes:usize,clock:u64}
pub(super) struct Cache {limit:usize,state:Mutex<State>}
impl Cache {
 fn new(limit:usize)->Self {Self {limit,state:Mutex::new(State::default())}}
 pub(super) fn shared()->&'static Self {static C:OnceLock<Cache>=OnceLock::new();static REGISTER:std::sync::Once=std::sync::Once::new();let cache=C.get_or_init(||Cache::new(crate::resources::budget("packet_plans")));REGISTER.call_once(||crate::resources::register("packet_plans",move|limit|cache.trim(limit)));cache}
 fn trim(&self,limit:usize)->crate::resources::Occupancy {let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());while s.bytes>limit||s.entries.len()>crate::resources::entry_limit("RIVER_S3_PACKET_PLAN_CACHE_ENTRIES",limit){let Some((_,key))=s.lru.pop_first()else{break;};if let Some(old)=s.entries.remove(&key){s.bytes-=old.bytes;crate::resources::evict("packet_plans",1);}}crate::resources::Occupancy{bytes:s.bytes,entries:s.entries.len(),..Default::default()}}
 fn tick(s:&mut State)->u64 {
  if s.clock==u64::MAX {s.entries.clear();s.lru.clear();s.bytes=0;s.clock=0;}
  s.clock+=1;s.clock
 }
 pub(super) fn get_or_build(&self,h:&Header,tile:u16,prefix:Option<u8>)->anyhow::Result<(Plan,bool)> {
  let key=Key::new(h,tile,prefix);
  if crate::resources::effective("packet_plans",self.limit)>0 {
   let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());let now=Self::tick(&mut s);
   if let Some(entry)=s.entries.get_mut(&key) {
    crate::resources::hit("packet_plans",entry.bytes);let old=entry.touched;entry.touched=now;let plan=entry.plan.clone();
    s.lru.remove(&old);s.lru.insert(now,key);return Ok((plan,true));
   }
  }
  crate::resources::miss("packet_plans");
  // Build outside the cache lock; unrelated images must not wait for geometry.
  let (packets,total)=packet_order(h,tile,prefix)?;let plan=(Arc::new(packets),total);
  let bytes=plan.0.capacity().saturating_mul(std::mem::size_of::<Packet>())
   .saturating_add(key.cod.capacity()*2).saturating_add(384);
  if crate::resources::effective("packet_plans",self.limit)>0 && bytes<=crate::resources::effective("packet_plans",self.limit) {
   let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());let now=Self::tick(&mut s);
   if let Some(old)=s.entries.remove(&key) {s.bytes-=old.bytes;crate::resources::evict("packet_plans",1);s.lru.remove(&old.touched);}
   while s.bytes.saturating_add(bytes)>crate::resources::effective("packet_plans",self.limit) || s.entries.len()>=crate::resources::entry_limit("RIVER_S3_PACKET_PLAN_CACHE_ENTRIES",crate::resources::effective("packet_plans",self.limit)) {
    let Some((_,old_key))=s.lru.pop_first() else{break;};
    if let Some(old)=s.entries.remove(&old_key) {s.bytes-=old.bytes;crate::resources::evict("packet_plans",1);}
   }
   s.bytes+=bytes;s.lru.insert(now,key.clone());s.entries.insert(key,Entry {plan:plan.clone(),bytes,touched:now});
  }
  Ok((plan,false))
 }
}
#[cfg(test)]mod tests {
 use super::*;use crate::decoder::NativeTileLayoutHint;
 fn header()->Header {Header {image_origin:(0,0),width:1024,height:1024,components:3,display_rgb_only:false,bit_depth:12,levels:4,layers:10,
  layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:1,rows:1},
  progression:2,is_ht:false,cod:vec![1,2,0,10,1,3,4,4,4,1,0x55,0x55,0x55,0x55],
  first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:false,payload_digest:[0;32]}}
 #[test]fn immutable_geometry_is_shared_but_distinct_orders_miss() {
  let c=Cache::new(8*1024*1024);let h=header();let (a,hit)=c.get_or_build(&h,0,None).unwrap();assert!(!hit);
  let (b,hit)=c.get_or_build(&h,0,None).unwrap();assert!(hit && Arc::ptr_eq(&a.0,&b.0));
  let expected=packet_order(&h,0,None).unwrap();
  let tuples=|v:&[Packet]|v.iter().map(|p|(p.resolution,p.layer,p.component,p.x,p.y)).collect::<Vec<_>>();
  assert_eq!(tuples(&a.0),tuples(&expected.0));assert_eq!(a.1,expected.1);
  assert!(!c.get_or_build(&h,0,Some(0)).unwrap().1);
  let mut changed=h.clone();changed.progression=0;assert!(!c.get_or_build(&changed,0,None).unwrap().1);
  // Content/version changes don't alter geometry. No payload is stored here.
  let mut version=h.clone();version.payload_digest=[9;32];assert!(c.get_or_build(&version,0,None).unwrap().1);
 }
 #[test]fn disabled_and_evicted_plans_remain_bounded_and_active_leases_survive() {
  let h=header();let off=Cache::new(0);assert!(!off.get_or_build(&h,0,None).unwrap().1);assert!(!off.get_or_build(&h,0,None).unwrap().1);
  let (_,n)=packet_order(&h,0,None).unwrap();let budget=n*std::mem::size_of::<Packet>()*2+1024;
  let c=Cache::new(budget);let (lease,_)=c.get_or_build(&h,0,None).unwrap();
  for layers in 11..18 {let mut other=h.clone();other.layers=layers;other.cod[2..4].copy_from_slice(&layers.to_be_bytes());c.get_or_build(&other,0,None).unwrap();}
  assert_eq!(lease.0.len(),n);assert!(c.state.lock().unwrap().bytes<=budget);
 }
}
