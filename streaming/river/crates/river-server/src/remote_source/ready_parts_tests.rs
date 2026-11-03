use super::*;
fn reference(index:&PartIndex,selected:&HashSet<u16>)->Option<Vec<TilePart>> {
    let parts=index.parts.iter().filter(|part|selected.contains(&part.tile)).cloned().collect::<Vec<_>>();
    if !index.complete {
        for tile in selected {
            let matching=parts.iter().filter(|part|part.tile==*tile).collect::<Vec<_>>();
            let total=matching.first()?.total;
            if total==0 || matching.len()!=usize::from(total)
                || matching.iter().any(|part|part.total!=total || part.ordinal>=total)
                || matching.iter().map(|part|part.ordinal).collect::<HashSet<_>>().len()!=usize::from(total) {return None;}
        }
    }
    Some(parts)
}

#[test]
fn grouped_validation_preserves_reference_acceptance_and_order() {
 let mut seed=0x123456789abcdefu64;
 let mut random=||{seed=seed.wrapping_mul(6364136223846793005).wrapping_add(1);(seed>>32) as u32};
 for _ in 0..2000 {
  let mut parts=vec![];
  for tile in 0..16u16 {let total=(random()%6) as u8;for ordinal in 0..total {
   if random()%11==0 {continue;}
   parts.push(TilePart{tile,offset:random() as u64,length:32,ordinal,total});
   if random()%17==0 {parts.push(parts.last().unwrap().clone());}
  }}
  if random()%3==0 {parts.reverse();}
  if let Some(part)=parts.get_mut((random() as usize)%64) {if random()%2==0 {part.total=0;}else{part.ordinal=255;}}
  let selected=(0..18u16).filter(|_|random()%3!=0).collect();
  let index=PartIndex{parts,cursor:0,complete:random()%7==0};
  assert_eq!(ready_parts(&index,&selected),reference(&index,&selected));
 }
 let parts=(0..9).flat_map(|ordinal|(0..12).map(move|tile|TilePart{tile,offset:u64::from(ordinal)*1000+u64::from(tile),length:14,ordinal,total:9})).collect();
 let index=PartIndex{parts,cursor:0,complete:false};let selected=HashSet::from([1,3,7]);
 let actual=ready_parts(&index,&selected).unwrap();assert_eq!(Some(actual.clone()),reference(&index,&selected));assert!(actual.windows(2).all(|w|w[0].offset<w[1].offset));
}
#[test]
#[ignore="Release-only metadata microbenchmark; not HTTP latency evidence"]
fn reused_index_validation_benchmark() {
 let index=PartIndex{parts:(0..4459).flat_map(|tile|(0..9).map(move|ordinal|TilePart{tile,offset:u64::from(tile)*1000+u64::from(ordinal),length:14,ordinal,total:9})).collect(),cursor:0,complete:false};
 let selected=(0..4459).collect();assert_eq!(ready_parts(&index,&selected),reference(&index,&selected));
 for pair in 0..6 {for grouped in if pair%2==0 {[false,true]}else{[true,false]} {
  let started=Instant::now();let result=if grouped {ready_parts(&index,&selected)}else{reference(&index,&selected)};std::hint::black_box(result);
  eprintln!("READY_INDEX_BENCH {}",serde_json::json!({"pair":pair,"grouped":grouped,"parts":index.parts.len(),"tiles":4459,"ms":started.elapsed().as_secs_f64()*1000.0}));
 }}
}
