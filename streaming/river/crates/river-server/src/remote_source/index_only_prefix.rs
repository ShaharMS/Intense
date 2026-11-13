//! PLT pointers may precede packet data in zero-body tile-parts. Keep only
//! the requested prefix lengths; never build the entire huge-tile index.
use super::*;

impl RemoteSource {
 pub(super) fn prepare_index_only_prefix(&self,payload:&HeaderPayload,reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  if !std::env::var("RIVER_S3_INDEX_ONLY_PREFIX").is_ok_and(|v|v=="true" || v=="1") {return Ok(None);}
  let settings=crate::read_policy::settings();
  let mut header_decision=None;
  let (window,window_budget)=if let Some(policy)=settings.index_headers.as_ref() {
   let plan=crate::read_policy::index_header_plan(policy,&settings,&self.read_profile.snapshot());let window=u64::from(plan.window_bytes);header_decision=Some(plan);(window,policy.max_extra_bytes)
  }else {(env_limit("RIVER_S3_INDEX_ONLY_HEADER_WINDOW_BYTES",0).min(8*1024*1024) as u64,u64::MAX)};
  let previous=context.trace_index;let result=self.index_only_prefix_inner(payload,reduction,layers,context,window,window_budget);context.trace_index=previous;if matches!(&result,Ok(Some(_))) {if let Some(plan)=header_decision {self.read_profile.index_header_decision(plan);}}result
 }
 fn index_only_prefix_inner(&self,payload:&HeaderPayload,reduction:u8,layers:u32,context:&mut ReadContext,window:u64,window_budget:u64)->Result<Option<PreparedSubset>> {
  let h=&self.header;
  if !h.prefix_compatible || h.is_ht || h.layers!=1 || layers>1 || reduction==0 || reduction>=h.levels
   || h.cod[9]!=1 || !matches!(h.progression,1|2) || h.image_origin!=(0,0)
   || h.layout.origin_x!=0 || h.layout.origin_y!=0 || h.layout.columns!=1 || h.layout.rows!=1 {return Ok(None);}
  let scale=1u32<<reduction;let width=h.width.div_ceil(scale);let height=h.height.div_ceil(scale);
  if u64::from(width)*u64::from(height)>4*1024*1024 {return Ok(None);}
  let (wanted,_)=multipart_prefix::counts(h,0,h.levels-1-reduction)?;
  if wanted==0 || wanted>524288 {return Ok(None);}
  // An explicitly single-part SOT cannot use the index-only chain.
  let sot=self.read(h.first_part..h.first_part+12,context)?;
  if sot[..4]==[0xff,0x90,0,10] && sot[11]==1 {return Ok(None);}
  let first=self.part_at(h.first_part,context)?;
  if first.tile!=0 || first.ordinal!=0 || first.total==1 || first.offset+first.length>=h.codestream_end.saturating_sub(2) {return Ok(None);}
  // This is a planning filter only; the full marker chain is still validated.
  if self.read(first.offset+first.length-2..first.offset+first.length,context)?!=[0xff,0x93] {return Ok(None);}
  ensure!(context.admit_metadata(wanted.saturating_mul(8).saturating_add(32768)),"index-only prefix exceeds metadata budget");
  let window=if window>0 && window<=window_budget && (window>1024*1024 || context.admit_metadata(window as usize)) {window}else{0};
  // Default sparse reads preserve source bandwidth; a constrained metadata budget also falls back to sparse reads.
  // A dense header still follows every declared marker; it never trusts guessed SODs.
  let mut window_used=0u64;let mut part=first.clone();let mut lengths=Vec::with_capacity(wanted);let mut index_parts=0;let mut preserved=vec![];let mut spans=vec![];let mut copied=0u64;
  for ordinal in 0..64usize {
   context.check()?;
   context.trace_index=Some(ordinal);
   ensure!(part.tile==0 && usize::from(part.ordinal)==ordinal,"index-only tile-part ordinal is not consecutive");
   let mut streamed_end=0;let mut ahead:Option<(u64,Vec<u8>)>=None;let mut at=part.offset+12;let mut segments=BTreeMap::new();let mut data=None;
   for marker_index in 0..4096 {
    context.check()?;ensure!(marker_index<4095,"index-only header has too many markers");
    let block_at=at/self.block_size*self.block_size;
    let cached=context.pinned.get(&block_at).cloned().or_else(||context.recent.get(self.block_size,block_at)).or_else(||self.cache.get(&self.cache_key,self.block_size,block_at));
    let cached_sod=cached.as_ref().and_then(|bytes|bytes.get((at-block_at) as usize..(at-block_at) as usize+2)).is_some_and(|bytes|bytes==[0xff,0x93]);
    if !cached_sod && window>0 && at+5>streamed_end && ahead.as_ref().is_none_or(|(base,bytes)|at<*base || at+5>*base+bytes.len() as u64) {
     let start=if window>1024*1024 {at/self.block_size*self.block_size}else{at};let end=start.saturating_add(window).min(part.offset+part.length);
     if end-start<=window_budget.saturating_sub(window_used) {
      if window>1024*1024 {if context.admit_metadata(streamed_header::RETAINED_LIMIT) {let body_bytes=if lengths.len()==wanted {lengths.iter().copied().sum()}else{0};self.prefetch_streamed_header(at,end,context,body_bytes)?;window_used+=end-start;streamed_end=end;}}
      else {let bytes=self.read(at..end,context)?;window_used+=end-at;ahead=Some((at,bytes));}
     }
    }
    let head=self.read_tile_header_bytes(at..at+4,&ahead,context)?;let code=be16(&head,0)?;
    if code==0xff93 {data=Some(at+2);break;}
    let n=u64::from(be16(&head,2)?);
    ensure!(n>=2 && at+2+n<=part.offset+part.length,"invalid index-only header marker extent");
    if code==0xff58 {
     ensure!(n>=3,"invalid index-only PLT marker");let z=self.read_tile_header_bytes(at+4..at+5,&ahead,context)?[0];
     ensure!(segments.insert(z,at+5..at+n+2).is_none(),"duplicate index-only PLT sequence");
    }else if code==0xff64 {
     ensure!(n>=4,"truncated index-only comment");
    }else {
     // Later coding changes must not be silently projected using main COD.
     if matches!(code,0xff52|0xff5f|0xff60|0xff61) {return Ok(None);}
     let bytes=self.read_tile_header_bytes(at..at+n+2,&ahead,context)?;
     if code==0xff53 && !identical_coc(&bytes[4..],h.components,&h.cod) {return Ok(None);}
     if ordinal>0 {return Ok(None);}
     ensure!(context.admit_metadata(bytes.len()),"index-only coding headers exceed metadata budget");
     preserved.extend(bytes);ensure!(preserved.len()<=MAX_HEADER,"index-only coding headers exceed limit");
    }
    at+=n+2;
   }
   let start=data.context("index-only header has no SOD")?;
   // Validate sequence continuity even for lengths this request need not load.
   for (expected,(&z,_)) in segments.iter().enumerate() {ensure!(expected==usize::from(z),"missing index-only PLT segment");}
   let mut value=0u64;let mut pending=false;
   for range in segments.values() {
    if lengths.len()>=wanted {break;}
    let need=u64::try_from((wanted-lengths.len()).saturating_mul(5))?;
    let bytes=self.read(range.start..range.end.min(range.start.saturating_add(need)),context)?;
    for byte in bytes {
     ensure!(value<=u64::from(u32::MAX)>>7,"index-only PLT length overflow");value=(value<<7)|u64::from(byte&127);pending=byte&128!=0;
     if !pending {
      ensure!(value>0,"invalid zero-length index-only packet");
      lengths.push(value);value=0;
      if lengths.len()==wanted {break;}
     }
    }
   }
   ensure!(!pending,"incomplete index-only packet length");
   let body=part.offset+part.length-start;
   if body==0 {
    if index_parts==0 && segments.is_empty() {return Ok(None);}
    index_parts+=1;
   }else {
    if index_parts==0 {return Ok(None);}
    if lengths.len()!=wanted {return Ok(None);}
    let needed=lengths.iter().try_fold(0u64,|n,&v|n.checked_add(v)).context("index-only prefix byte overflow")?;
    if needed>16*1024*1024 {return Ok(None);}
    let take=body.min(needed-copied);spans.push((true,start..start+take));copied+=take;
    if copied==needed {
     let Some(mut main)=single_prefix::lowest_header(&payload.main,h,reduction)? else{return Ok(None);};
     let Some(headers)=single_prefix::lowest_header_segments(&preserved,h,reduction,false)? else{return Ok(None);};
     let bytes=self.selected_payload(&spans,context)?;let mut sot=self.read(first.offset..first.offset+12,context)?;
     sot[6..10].copy_from_slice(&u32::try_from(14+headers.len()+bytes.len())?.to_be_bytes());sot[10]=0;sot[11]=1;
     main.extend(sot);main.extend(headers);main.extend([0xff,0x93]);main.extend(bytes);main.extend([0xff,0xd9]);
     let jp2=!payload.container.is_empty();
     let bytes=if jp2 {let mut c=payload.container.clone();resize_jp2(&mut c,width,height)?;c.extend(u32::try_from(main.len()+8)?.to_be_bytes());c.extend(b"jp2c");c.extend(main);c}else{main};
     return Ok(Some(PreparedSubset {view_reduction:reduction,technical_origin:None,bytes,jp2,route:"s3-index-only-prefix",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0}));
    }
   }
   let next=part.offset+part.length;
   ensure!(next<h.codestream_end.saturating_sub(2),"index-only prefix reaches EOC before packet data");
   part=self.part_at(next,context)?;
  }
  bail!("index-only prefix exceeds tile-part traversal limit")
 }
}

#[cfg(test)] mod tests {
 use super::*;
 #[tokio::test]
 async fn pointers_in_empty_parts_project_later_packet_data_and_reject_late_coding_changes() {
  let runtime=tokio::runtime::Handle::current();
  tokio::task::spawn_blocking(move || {
   let marker=|code:u16,v:&[u8]| {let mut out=code.to_be_bytes().to_vec();out.extend(((v.len()+2) as u16).to_be_bytes());out.extend(v);out};
   for (late,(window,budget)) in [false,true].into_iter().flat_map(|late|[(0,u64::MAX),(1048576,u64::MAX),(1048576,0)].map(|window|(late,window))) {
    let mut siz=vec![0,0];for n in [32u32,32,0,0,32,32,0,0] {siz.extend(n.to_be_bytes());}siz.extend([0,3,7,1,1,7,1,1,7,1,1]);
    let cod=vec![5,2,0,1,0,1,2,2,0,1,0x44,0x44];
    let mut data=vec![0xff,0x4f];data.extend(marker(0xff51,&siz));data.extend(marker(0xff52,&cod));data.extend(marker(0xff5c,&[0x40,0x40,0x48,0x48,0x50]));
    let mut plt=vec![0];plt.extend(vec![3;15]);let header=marker(0xff58,&plt);
    data.extend([0xff,0x90,0,10,0,0]);data.extend(((14+header.len()) as u32).to_be_bytes());data.extend([0,0]);data.extend(header);data.extend([0xff,0x93]);
    data.extend([0xff,0x90,0,10,0,0,0,0,0,0,1,0]);if late {data.extend(marker(0xff52,&cod));}data.extend([0xff,0x93]);for _ in 0..15 {data.extend([0,0xff,0x92]);}data.extend([0xff,0xd9]);
    let directory=tempfile::tempdir().unwrap();std::fs::write(directory.path().join("index.j2k"),data).unwrap();let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(directory.path()).unwrap());let location=Path::from("index.j2k");let meta=runtime.block_on(store.head(&location)).unwrap();
    let cache=Arc::new(RangeCache {limit:64*1024*1024,state:Mutex::new(CacheState::default())});let (source,_)=RemoteSource::open("index-parts".into(),format!("index-parts-{}",uuid::Uuid::new_v4()),store,location,meta,runtime.clone(),cache).unwrap();
    let mut context=ReadContext::new(None);let payload=source.acquire_header(&mut context).unwrap();let view=source.index_only_prefix_inner(&payload,1,0,&mut context,window,budget).unwrap();
    if late {assert!(view.is_none());}else {let view=view.unwrap();assert_eq!(view.route,"s3-index-only-prefix");assert_eq!(view.view_reduction,1);assert!(view.bytes.ends_with(&[0,255,146,0,255,146,0,255,146,255,217]));}
   }
  }).await.unwrap();
 }
}
