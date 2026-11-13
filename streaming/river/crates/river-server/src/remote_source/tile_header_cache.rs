//! Bounded, version-keyed immutable parsed headers. No source bytes are written.
use super::{ParsedTileHeader,TilePart};
use std::{collections::{HashMap,BTreeMap},sync::{Mutex,OnceLock}};
struct Entry {part:TilePart,header:ParsedTileHeader,bytes:usize,touched:u64}
#[derive(Default)]struct State {entries:HashMap<(String,u64),Entry>,lru:BTreeMap<u64,(String,u64)>,bytes:usize,clock:u64}
pub(super) struct Cache {limit:usize,state:Mutex<State>}
impl Cache {
 fn new(limit:usize)->Self {Self {limit,state:Mutex::new(State::default())}}
 pub fn shared()->&'static Self {static C:OnceLock<Cache>=OnceLock::new();static REGISTER:std::sync::Once=std::sync::Once::new();let cache=C.get_or_init(||Cache::new(crate::resources::budget("parsed_headers")));REGISTER.call_once(||crate::resources::register("parsed_headers",move|limit|cache.trim(limit)));cache}
 fn trim(&self,limit:usize)->crate::resources::Occupancy {let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());while s.bytes>limit||s.entries.len()>crate::resources::entry_limit("RIVER_S3_PARSED_HEADER_CACHE_ENTRIES",limit){let Some((_,key))=s.lru.pop_first()else{break;};if let Some(old)=s.entries.remove(&key){s.bytes-=old.bytes;crate::resources::evict("parsed_headers",1);}}crate::resources::Occupancy{bytes:s.bytes,entries:s.entries.len(),..Default::default()}}
 pub fn get(&self,id:&str,part:&TilePart)->Option<ParsedTileHeader> {
  if crate::resources::effective("parsed_headers",self.limit)==0 {return None;}
  let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());
  if s.clock==u64::MAX {*s=State::default();}s.clock+=1;let now=s.clock;
  let key=(id.to_owned(),part.offset);let Some(entry)=s.entries.get_mut(&key)else{crate::resources::miss("parsed_headers");return None;};crate::resources::hit("parsed_headers",entry.bytes);
  if entry.part!=*part {return None;}
  let old=entry.touched;entry.touched=now;let header=entry.header.clone();
  s.lru.remove(&old);s.lru.insert(now,key);Some(header)
 }
 /// A parsed header also proves its exact SOT on this immutable source version.
 /// The payload start lets callers preserve requested entropy read-ahead.
 pub fn indexed(&self,id:&str,offset:u64)->Option<(TilePart,u64)> {
  if crate::resources::effective("parsed_headers",self.limit)==0 {return None;}
  let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());
  if s.clock==u64::MAX {*s=State::default();}s.clock+=1;let now=s.clock;
  let key=(id.to_owned(),offset);let Some(entry)=s.entries.get_mut(&key)else{crate::resources::miss("parsed_headers");return None;};crate::resources::hit("parsed_headers",entry.bytes);
  let old=entry.touched;entry.touched=now;let result=(entry.part.clone(),entry.header.1);
  s.lru.remove(&old);s.lru.insert(now,key);Some(result)
 }
 pub fn put(&self,id:&str,part:&TilePart,header:&ParsedTileHeader) {
  let bytes=id.len()*2+header.0.capacity()+header.2.as_ref().map_or(0,|v|v.capacity()*8)+384;
  if bytes>crate::resources::effective("parsed_headers",self.limit) || bytes>4*1024*1024 {return;}
  let mut s=self.state.lock().unwrap_or_else(|e|e.into_inner());let key=(id.to_owned(),part.offset);
  if s.clock==u64::MAX {*s=State::default();}
  if let Some(old)=s.entries.remove(&key) {s.bytes-=old.bytes;crate::resources::evict("parsed_headers",1);s.lru.remove(&old.touched);}
  while s.bytes+bytes>crate::resources::effective("parsed_headers",self.limit) || s.entries.len()>=crate::resources::entry_limit("RIVER_S3_PARSED_HEADER_CACHE_ENTRIES",crate::resources::effective("parsed_headers",self.limit)) {
   let Some((_,key))=s.lru.pop_first() else{break;};
   s.bytes-=s.entries.remove(&key).unwrap().bytes;
  }
  s.clock+=1;let now=s.clock;s.lru.insert(now,key.clone());s.entries.insert(key,Entry {part:part.clone(),header:header.clone(),bytes,touched:now});s.bytes+=bytes;
 }
}
#[cfg(test)]mod tests {
 use super::*;use std::sync::Arc;
 fn part(offset:u64)->TilePart {TilePart {tile:0,offset,length:100,ordinal:0,total:1}}
 fn header(n:usize)->ParsedTileHeader {(vec![1,2],12,Some(Arc::new(vec![9;n])),true)}
 #[test]fn immutable_lengths_are_shared_and_version_and_extent_mismatches_miss() {
  let c=Cache::new(100000);let p=part(100);let h=header(8192);c.put("version-a",&p,&h);let a=c.get("version-a",&p).unwrap();
  assert!(Arc::ptr_eq(h.2.as_ref().unwrap(),a.2.as_ref().unwrap()));assert!(c.get("version-b",&p).is_none());
  let mut changed=p.clone();changed.length+=1;assert!(c.get("version-a",&changed).is_none());
  assert_eq!(c.indexed("version-a",100).unwrap(),(p,12));
  assert!(c.indexed("version-b",100).is_none());assert!(c.indexed("version-a",101).is_none());
 }
 #[test]fn eviction_is_bounded_and_active_leases_survive() {
  let c=Cache::new(100000);let h=header(8192);c.put("a",&part(100),&h);let lease=c.get("a",&part(100)).unwrap();c.put("b",&part(200),&h);
  assert!(c.get("a",&part(100)).is_none());assert!(c.get("b",&part(200)).is_some());assert_eq!(lease.2.as_ref().unwrap()[0],9);assert!(c.state.lock().unwrap().bytes<=c.limit);
  let off=Cache::new(0);off.put("a",&part(100),&h);assert!(off.get("a",&part(100)).is_none());
 }
}
