use std::collections::BTreeMap;

pub fn preferred_edge(width:u32,height:u32,native_width:u32,native_height:u32,origin:(u32,u32),range_latency_us:u64)->u32 {
    if (5000..u64::MAX).contains(&range_latency_us) && native_width==1024 && native_height==1024 && origin==(0,0)
        && u64::from(width.div_ceil(1024))*u64::from(height.div_ceil(1024))<=4096 {1024} else {2048}
}

pub fn batches(tiles:&[(u32,u32)],batch_size:usize)->Vec<Vec<(u32,u32)>> {
    let batch_size=batch_size.clamp(1,32);
    let mut rows=BTreeMap::<u32,Vec<(u32,u32)>>::new();
    for &tile in tiles {rows.entry(tile.1).or_default().push(tile);}
    let mut batches=Vec::new();
    for row in rows.into_values() {
        let first=if batches.is_empty() {8.min(batch_size).min(row.len())} else {0};
        if first>0 {batches.push(row[..first].to_vec());}
        batches.extend(row[first..].chunks(batch_size).map(|batch|batch.to_vec()));
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_row_patches_keep_the_existing_frame_bound_and_layout_fallback() {
        assert_eq!(preferred_edge(40980,10980,1024,1024,(0,0),10000),1024);
        assert_eq!(preferred_edge(100000,100000,1024,1024,(0,0),10000),2048);
        assert_eq!(preferred_edge(40980,10980,512,512,(0,0),10000),2048);
        assert_eq!(preferred_edge(40980,10980,1024,1024,(1,0),10000),2048);
        for latency in [1000,4999,u64::MAX] {assert_eq!(preferred_edge(40980,10980,1024,1024,(0,0),latency),2048);}
        assert_eq!(preferred_edge(40980,10980,1024,1024,(0,0),5000),1024);
    }
    #[test]
    fn first_batch_is_small_and_every_batch_stays_rectangular_and_in_order() {
        let tiles=(0..3).flat_map(|y|(0..41).map(move|x|(x,y))).collect::<Vec<_>>();
        for size in [0,1,8,32,usize::MAX] {
            let batches=batches(&tiles,size);
            assert!(batches[0].len()<=8);
            assert_eq!(batches.concat(),tiles);
            for batch in batches {
                assert!(batch.len()<=size.clamp(1,32));
                assert!(batch.iter().all(|tile|tile.1==batch[0].1));
                assert!(batch.windows(2).all(|pair|pair[1].0==pair[0].0+1));
            }
        }
    }
}
