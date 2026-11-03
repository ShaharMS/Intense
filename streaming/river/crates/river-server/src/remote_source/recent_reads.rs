//! Bounded locality for one version-pinned reader, independent of shared-cache
//! retention. Only bytes already requested are remembered; no read-ahead.
use std::{collections::VecDeque,sync::Arc};
const MAX_BLOCKS:usize=16;
const MAX_BYTES:usize=16*1024;
#[derive(Default)]
pub(super) struct RecentReads {blocks:VecDeque<(u64,u64,Arc<[u8]>)>,bytes:usize}
impl RecentReads {
    pub fn contains(&self,size:u64,offset:u64)->bool {self.blocks.iter().any(|(s,o,_)|*s==size && *o==offset)}
    pub fn get(&mut self,size:u64,offset:u64)->Option<Arc<[u8]>> {
        let position=self.blocks.iter().rposition(|(s,o,_)|*s==size && *o==offset)?;
        let block=self.blocks.remove(position)?;let data=block.2.clone();self.blocks.push_back(block);Some(data)
    }
    pub fn remember(&mut self,size:u64,blocks:&[(u64,Arc<[u8]>)]) {
        for (offset,data) in blocks.iter().rev().take(MAX_BLOCKS).rev() {
            if data.len()>MAX_BYTES {continue;}
            if let Some(position)=self.blocks.iter().position(|(s,o,_)|*s==size && *o==*offset) {
                self.bytes-=self.blocks.remove(position).unwrap().2.len();
            }
            while self.blocks.len()>=MAX_BLOCKS || self.bytes+data.len()>MAX_BYTES {
                let Some(old)=self.blocks.pop_front() else {break;};self.bytes-=old.2.len();
            }
            self.bytes+=data.len();self.blocks.push_back((size,*offset,data.clone()));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacent_reads_survive_shared_eviction_with_a_bounded_working_set() {
        let mut recent=RecentReads::default();let first:Arc<[u8]>=Arc::from(vec![7;1024]);
        recent.remember(1024,&[(0,first.clone())]);drop(first);
        let lease=recent.get(1024,0).unwrap();assert_eq!(lease[0],7);
        for n in 1..=16 {recent.remember(1024,&[(n*1024,Arc::from(vec![n as u8;1024]))]);}
        assert!(recent.get(1024,0).is_none());assert_eq!(lease[0],7);
        assert_eq!(recent.bytes,MAX_BYTES);assert_eq!(recent.blocks.len(),MAX_BLOCKS);
        recent.get(1024,1024).unwrap();recent.remember(1024,&[(17*1024,Arc::from(vec![17;1024]))]);
        assert!(recent.contains(1024,1024));assert!(!recent.contains(1024,2048));
    }
    #[test]
    fn block_geometry_and_large_reads_do_not_break_the_memory_bound() {
        let mut recent=RecentReads::default();recent.remember(1024,&[(0,Arc::from(vec![1;1024]))]);
        recent.remember(256,&[(0,Arc::from(vec![2;256]))]);
        assert_eq!(recent.get(1024,0).unwrap()[0],1);assert_eq!(recent.get(256,0).unwrap()[0],2);
        let many=(0..1024).map(|n|(n*1024,Arc::from(vec![3;1024]))).collect::<Vec<_>>();recent.remember(1024,&many);
        assert!(recent.blocks.len()<=MAX_BLOCKS && recent.bytes<=MAX_BYTES);
        recent.remember(32768,&[(0,Arc::from(vec![4;32768]))]);assert!(!recent.contains(32768,0));
    }
}
