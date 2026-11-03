//! Keep tiny previews on the in-process path, while large compatible LL
//! rasters can use direct block decoding even with a modest packet count.
pub(super) fn prefer(packets:usize,width:u32,height:u32,direct_compatible:bool,minimum:Option<usize>,direct_enabled:bool)->bool {
    if let Some(minimum)=minimum {return packets>=minimum;}
    packets>=16384 || (direct_enabled && direct_compatible && u64::from(width)*u64::from(height)>=65536)
}
#[cfg(test)] mod tests {
    use super::prefer;
    #[test] fn explicit_threshold_and_legacy_mode_keep_their_contract() {
        assert!(!prefer(1000,641,172,true,Some(16384),true));
        assert!(prefer(1,1,1,false,Some(0),false));
        assert!(!prefer(1000,641,172,true,None,false));
        assert!(prefer(16384,1,1,false,None,false));
    }
    #[test] fn large_rasters_need_direct_compatible_metadata_to_lower_the_threshold() {
        assert!(prefer(1000,641,172,true,None,true));
        assert!(!prefer(1000,641,172,false,None,true));
        assert!(!prefer(1000,255,256,true,None,true));
        assert!(prefer(1000,256,256,true,None,true));
    }
}

pub(crate) fn prefer_coarse(packets:usize,width:u32,height:u32,region:(u32,u32,u32,u32),minimum:Option<usize>)->bool {
 if let Some(minimum)=minimum {return packets>=minimum;}
 let pixels=u64::from(width)*u64::from(height);
 let requested=u64::from(region.2.saturating_sub(region.0))*u64::from(region.3.saturating_sub(region.1));
 pixels>=65536 && requested.saturating_mul(2)>=pixels
}
#[cfg(test)] mod coarse_tests {
 use super::prefer_coarse;
 #[test] fn coarse_worker_respects_region_cost_and_explicit_override() {
  assert!(prefer_coarse(1920,1024,2560,(0,0,1024,2560),None));
  assert!(!prefer_coarse(1920,1024,2560,(512,2048,1024,2560),None));
  assert!(!prefer_coarse(20000,1024,2560,(0,0,256,256),None));
  assert!(!prefer_coarse(10,128,128,(0,0,128,128),None));
  assert!(prefer_coarse(1920,1024,2560,(512,2048,1024,2560),Some(0)));
  assert!(!prefer_coarse(1920,1024,2560,(0,0,1024,2560),Some(16384)));
 }
}
