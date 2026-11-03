//! Bounded PLT traversal and a compact, entropy-preserving single-tile LL view.
//! Packet counts are streamed; high-resolution packet arrays are never built.
use super::*;

impl RemoteSource {
 /// A validated resolution-ordered PLT chain can finish a whole-image preview
 /// before discovering the tile's finer parts. No TLM or part/resolution mapping
 /// is inferred: packet counts and exact payload extents establish the prefix.
 pub(super) fn prepare_sequential_plt_prefix(&self,payload:&HeaderPayload,reduction:u8,layers:u32,
    context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let h=&self.header;
  if !std::env::var("RIVER_S3_SINGLE_PREFIX").is_ok_and(|v|v=="true" || v=="1")
   || !payload.tlm.is_empty() || !payload.jpip.is_empty() || !sequential_full_quality(h,layers) || h.is_ht
   || reduction==0 || reduction>=h.levels || h.cod[9]!=1 || h.cod[0]&!1!=0
   || h.image_origin!=(0,0) || h.layout.origin_x!=0 || h.layout.origin_y!=0
   || h.layout.columns!=1 || h.layout.rows!=1 {return Ok(None);}
  let scale=1u32<<reduction;
  let (width,height)=(h.width.div_ceil(scale),h.height.div_ceil(scale));
  if u64::from(width)*u64::from(height)>4*1024*1024 {return Ok(None);}
  let first=self.part_at(h.first_part,context)?;
  if first.tile!=0 || first.ordinal!=0 || first.total==1
   || (first.total==0 && first.offset+first.length>=h.codestream_end.saturating_sub(2)) {return Ok(None);}
  let (wanted,total)=multipart_prefix::counts(h,0,h.levels-1-reduction)?;
  if wanted==0 || wanted>MAX_PACKETS {return Ok(None);}
  let mut at=h.first_part;let mut covered=0usize;let mut bytes=Vec::new();let mut headers=Vec::new();
  let mut parts=0usize;let mut complete=false;
  for ordinal in 0..256usize {
   context.check()?;
   let part=self.part_at(at,context)?;
   if part.tile!=0 || usize::from(part.ordinal)!=ordinal
    || (part.total>0 && first.total>0 && part.total!=first.total) {return Ok(None);}
   let (coding,body,lengths,compatible)=self.tile_header(&part,context)?;
   if !compatible || (ordinal>0 && !coding.is_empty()) {return Ok(None);}
   let Some(lengths)=lengths else{return Ok(None);};
   if lengths.is_empty() {return Ok(None);}
   let end=part.offset.checked_add(part.length).context("sequential PLT part overflow")?;
   let take=sequential_plt_take(&lengths,covered,wanted,total,end.checked_sub(body).context("invalid sequential PLT body")?)?;
   read_trace::index_entry_from(context.trace_id,&self.id,ordinal,part.tile,part.offset..end,self.block_size,"validatedSequentialPlt");
   let take_size=usize::try_from(take).context("sequential PLT prefix exceeds address space")?;
   if bytes.len().checked_add(take_size).is_none_or(|size|size>16*1024*1024) {
    tracing::info!(context=context.trace_id,optimization="sequential_plt_prefix",reason="compact_prefix_byte_limit",prefix_bytes=bytes.len(),next_part_bytes=take_size,action="alternate_read_path","Compact prefix exceeds its bounded buffer");
    return Ok(None);
   }
   if ordinal==0 {headers=coding;}
   let done=covered+lengths.len()>=wanted;
   // The preceding payload is required. A small, bounded extension can also
   // contain the next SOT/PLT, avoiding a separate dependent metadata GET.
   let lookahead=if done {0}else{take.min(16*1024)};
   let read_end=body.checked_add(take).and_then(|n|n.checked_add(lookahead)).context("sequential PLT read overflow")?.min(h.codestream_end);
   let retain=if self.cache.limit<16*1024*1024 {
    // A later part may exceed the compact-prefix cap or change coding style.
    // Preserve useful bytes for the ordinary fallback,within the existing
    // request-local8MiBpin budget. Decline before fetching an unretainable span.
    let span=body..read_end;
    if !context.can_pin_ranges(self.block_size,&[(true,span.clone())]) {
     tracing::info!(context=context.trace_id,optimization="sequential_plt_prefix",reason="request_pin_budget",action="alternate_read_path","Prefix fallback preserves bytes within the request pin budget");
     return Ok(None);
    }
    Some(span)
   }else{(lookahead>0).then_some(body+take..read_end)};
   let data=self.selected_payload_retaining(&[(true,body..read_end)],retain,context)?;
   ensure!(data.len()>=take_size,"truncated sequential PLT payload");
   bytes.extend_from_slice(&data[..take_size]);parts+=1;
   if done {complete=true;break;}
   covered=covered.checked_add(lengths.len()).context("sequential PLT packet overflow")?;
   at=end;
   if first.total>0 && parts>=usize::from(first.total) {return Ok(None);}
  }
  if !complete {return Ok(None);}
  let Some(mut main)=lowest_header(&payload.main,h,reduction)? else{return Ok(None);};
  let Some(headers)=lowest_header_segments(&headers,h,reduction,false)? else{return Ok(None);};
  // Tile0/ordinal0were already validated. Every emitted SOT field is known;
  // synthesizing it avoids rereading the opening block with a small byte cache.
  let mut sot=vec![0xff,0x90,0,10,0,0,0,0,0,0,0,1];
  sot[6..10].copy_from_slice(&u32::try_from(12+headers.len()+2+bytes.len())?.to_be_bytes());
  main.extend(sot);main.extend(headers);main.extend([0xff,0x93]);main.extend(bytes);main.extend([0xff,0xd9]);
  let jp2=!payload.container.is_empty();
  let bytes=if jp2 {let mut container=payload.container.clone();resize_jp2(&mut container,width,height)?;container.extend(u32::try_from(main.len()+8)?.to_be_bytes());container.extend(b"jp2c");container.extend(main);container}else{main};
  tracing::info!(context=context.trace_id,parts,packets=wanted,optimization="sequential_plt_prefix",action="skip_finer_tile_parts","PLT proves the requested resolution prefix");
  Ok(Some(PreparedSubset {view_reduction:reduction,technical_origin:None,bytes,jp2,route:"s3-sequential-plt-prefix",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0}))
 }
 pub(super) fn prepare_single_prefix(&self,payload:&HeaderPayload,reduction:u8,layers:u32,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let h=&self.header;
  if !std::env::var("RIVER_S3_SINGLE_PREFIX").is_ok_and(|v|v=="true" || v=="1")
   || !h.prefix_compatible || h.is_ht || h.layers!=1 || layers>1 || reduction==0 || reduction>=h.levels
   || h.image_origin!=(0,0) || h.layout.origin_x!=0 || h.layout.origin_y!=0
   || h.layout.columns!=1 || h.layout.rows!=1 {return Ok(None);}
  let scale=1u32<<reduction;
  if u64::from(h.width.div_ceil(scale))*u64::from(h.height.div_ceil(scale))>4*1024*1024 {return Ok(None);}
  let retained=u8::from(h.cod[9]==0 && h.cod[4]==1);
  let wanted_resolution=h.levels-1-reduction;
  let (wanted,total)=multipart_prefix::counts(h,0,wanted_resolution)?;
  // Preserve established ordinary paths. Large packet tables need streamed
  // traversal even for intermediate previews, rather than a full packet array.
  if wanted_resolution>0 && total<=MAX_PACKETS {return Ok(None);}
  let view_reduction=reduction-retained;let view_scale=1u32<<view_reduction;
  let width=h.width.div_ceil(view_scale);let height=h.height.div_ceil(view_scale);
  let part=self.part_at(h.first_part,context)?;
  if part.tile!=0 || part.ordinal!=0 || part.total>1 {return Ok(None);}
  if wanted>524288 {return Ok(None);}
  let (view_packets,_)=multipart_prefix::counts(h,0,wanted_resolution+retained)?;
  if view_packets>MAX_PACKETS {return Ok(None);}
  let slab_bytes=(u64::try_from(total)?.saturating_mul(5).saturating_add(16*1024)).next_power_of_two().clamp(64*1024,4*1024*1024);
  let mut at=part.offset+12;let mut headers=vec![];let mut expected=0u16;
  let (mut count,mut sum,mut prefix,mut value)=(0usize,0u64,0u64,0u64);let mut pending=false;let mut slab_end=0;
  let mut sot=self.read(part.offset..part.offset+12,context)?;
  for markers in 0..4096 {
   context.check()?;ensure!(markers<4095,"too many single-prefix header markers");
   if slab_end>0 && at+4>slab_end {
    slab_end=(at/self.block_size*self.block_size+slab_bytes).min(part.offset+part.length);
    let leases=self.prefetch_ranges(&[(true,at..slab_end)],context,usize::MAX)?;context.pinned.clear();context.pin_packet_blocks(leases);
   }
   let head=self.read(at..at+4,context)?;let marker=be16(&head,0)?;
   if marker==0xff93 {at+=2;break;}
   let length=u64::from(be16(&head,2)?);
   ensure!(length>=2 && at+2+length<=part.offset+part.length,"invalid single-prefix header extent");
   if marker==0xff64 {at+=length+2;continue;}
   if marker==0xff58 {
    if (slab_end==0 && length>=60*1024) || (slab_end>0 && at+length+2>slab_end) {
     // Large PLT series otherwise cause one dependent GET per64KiB marker.
     // One bounded slab covers their lengths and often the first LL bodies.
     slab_end=(at/self.block_size*self.block_size+slab_bytes).min(part.offset+part.length);
     let leases=self.prefetch_ranges(&[(true,at..slab_end)],context,usize::MAX)?;
     context.pinned.clear();context.pin_packet_blocks(leases);
    }
    let bytes=self.read(at+4..at+length+2,context)?;
    ensure!(expected<256 && bytes.first().copied()==Some(expected as u8),"invalid single-prefix PLT sequence");expected+=1;
    for &byte in &bytes[1..] {
     ensure!(value<=u64::from(u32::MAX)>>7,"single-prefix PLT length overflow");
     value=(value<<7)|u64::from(byte&127);pending=byte&128!=0;
     if !pending {
      ensure!(value>0,"invalid zero-length single-prefix PLT packet");
      count=count.checked_add(1).context("single-prefix packet count overflow")?;
      sum=sum.checked_add(value).context("single-prefix payload size overflow")?;
      if count<=wanted {prefix=prefix.checked_add(value).context("single-prefix selected size overflow")?;}
      value=0;
     }
    }
   }else{
    if matches!(marker,0xff52|0xff5f|0xff61) {return Ok(None);}
    let bytes=self.read(at..at+length+2,context)?;
    if marker==0xff53 && !identical_coc(&bytes[4..],h.components,&h.cod) {return Ok(None);}
    headers.extend(bytes);ensure!(headers.len()<=MAX_HEADER,"single-prefix coding header exceeds limit");
   }
   at+=length+2;
  }
  if expected==0 {return Ok(None);}
  // TNsot=0 leaves the chain length unknown. A complete PLT for this part
  // need not cover the entire tile: let multipart discovery continue.
  if part.total==0 && !pending && count<total {return Ok(None);}
  ensure!(!pending && count==total,"single-prefix PLT packet count disagrees with geometry");
  ensure!(sum==part.offset+part.length-at,"single-prefix PLT lengths do not cover payload");
  if prefix>16*1024*1024 {return Ok(None);}
  let Some(mut main)=lowest_header(&payload.main,h,reduction)? else{return Ok(None);};
  // Component quantization overrides need the same single-subband projection.
  let Some(headers)=lowest_header_segments(&headers,h,reduction,false)? else{return Ok(None);};
  // Use the payload planner's bounded contiguous ranges. The generic reader
  // otherwise serializes a multi-megabyte LL prefix into 1 MiB GETs.
  let mut bytes=self.selected_payload(&[(true,at..at+prefix)],context)?;
  for ordinal in wanted..view_packets {append_empty_packet(&mut bytes,h.cod[0],ordinal);}
  sot[10]=0;sot[11]=1;
  sot[6..10].copy_from_slice(&u32::try_from(12+headers.len()+2+bytes.len())?.to_be_bytes());
  main.extend(sot);main.extend(headers);main.extend([0xff,0x93]);main.extend(bytes);main.extend([0xff,0xd9]);
  let jp2=!payload.container.is_empty();
  let bytes=if jp2 {let mut container=payload.container.clone();resize_jp2(&mut container,width,height)?;container.extend(u32::try_from(main.len()+8)?.to_be_bytes());container.extend(b"jp2c");container.extend(main);container}else{main};
  Ok(Some(PreparedSubset {view_reduction,technical_origin:None,bytes,jp2,route:"s3-single-resolution-prefix",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0}))
 }
}

fn sequential_full_quality(h:&Header,layers:u32)->bool {
 h.packet_compatible && h.layers>0 && h.cod.get(4).is_some_and(|v|*v<=1)
  && h.cod.get(8).is_some_and(|v|v&0xc0==0)
  && (layers==0 || layers>=u32::from(h.layers))
  && if h.layers==1 {h.prefix_compatible}else{matches!(h.progression,1|2)}
}

fn sequential_plt_take(lengths:&[u64],covered:usize,wanted:usize,total:usize,body_bytes:u64)->Result<u64> {
 ensure!(covered<wanted && wanted<=total && !lengths.is_empty(),"invalid sequential PLT packet extent");
 ensure!(lengths.iter().all(|&n|n>0),"zero-length sequential PLT packet");
 ensure!(covered.checked_add(lengths.len()).is_some_and(|n|n<=total),"sequential PLT exceeds tile packet count");
 let sum=lengths.iter().try_fold(0u64,|sum,&n|sum.checked_add(n).context("sequential PLT length overflow"))?;
 ensure!(sum==body_bytes,"sequential PLT lengths do not cover tile-part payload");
 lengths.iter().take(wanted-covered).try_fold(0u64,|sum,&n|sum.checked_add(n).context("sequential PLT prefix overflow"))
}

pub(super) fn lowest_header(main:&[u8],h:&Header,reduce:u8)->Result<Option<Vec<u8>>> {
 let Some(mut rest)=lowest_header_segments(&main[2..],h,reduce,true)? else{return Ok(None);};
 let mut out=vec![0xff,0x4f];out.append(&mut rest);Ok(Some(out))
}
pub(super) fn lowest_header_segments(bytes:&[u8],h:&Header,reduce:u8,main:bool)->Result<Option<Vec<u8>>> {
 let retained=u8::from(h.cod[9]==0 && h.cod[4]==1);let kept=h.levels-1-reduce+retained;
 let mut at=0;let mut out=vec![];let scale=1u32<<(reduce-retained);
 while at<bytes.len() {
  let marker=be16(bytes,at)?;let length=usize::from(be16(bytes,at+2)?);
  ensure!(length>=2 && at+length+2<=bytes.len(),"invalid lowest-resolution header");
  let mut value=bytes[at+4..at+length+2].to_vec();
  match marker {
   0xff51=>{
    for (index,n) in [h.width.div_ceil(scale),h.height.div_ceil(scale),0,0,h.width.div_ceil(scale),h.height.div_ceil(scale),0,0].into_iter().enumerate() {value[2+index*4..6+index*4].copy_from_slice(&n.to_be_bytes());}
   }
   0xff52=>{
    ensure!(main && value.len()>=10,"invalid projected COD");value[5]=kept;
    if value[0]&1!=0 {value.truncate(11+usize::from(kept));}
    else{value.truncate(10);}
   }
   0xff53=>{
    ensure!(value.len()>=7,"invalid projected COC");value[2]=kept;
    if value[1]&1!=0 {ensure!(value.len()>=8+usize::from(kept),"missing projected COC precinct");value.truncate(8+usize::from(kept));}else{value.truncate(7);}
   }
   0xff5c=>{if value.is_empty(){return Ok(None);}let bands=1+3*usize::from(kept);let length=match value[0]&31 {0=>1+bands,1=>3,2=>1+2*bands,_=>return Ok(None)};if value.len()<length{return Ok(None);}value.truncate(length);}
   0xff5d=>{if value.len()<2{return Ok(None);}let bands=1+3*usize::from(kept);let length=match value[1]&31 {0=>2+bands,1=>4,2=>2+2*bands,_=>return Ok(None)};if value.len()<length{return Ok(None);}value.truncate(length);}
   0xff55|0xff57|0xff58|0xff64=>{at+=length+2;continue;}
   0xff5f|0xff60|0xff61=>return Ok(None),
   _=>{}
  }
  out.extend(marker.to_be_bytes());out.extend(u16::try_from(value.len()+2)?.to_be_bytes());out.extend(value);at+=length+2;
 }
 Ok(Some(out))
}

#[cfg(test)] mod tests {
 use super::*;
 #[test] fn sequential_plt_packet_accounting_stops_at_the_requested_prefix() {
  assert_eq!(sequential_plt_take(&[2,3,4],0,5,10,9).unwrap(),9);
  assert_eq!(sequential_plt_take(&[2,3,4],3,5,10,9).unwrap(),5);
  assert!(sequential_plt_take(&[2,3,4],0,5,10,8).is_err());
  assert!(sequential_plt_take(&[2,3,4],3,5,5,9).is_err());
  assert!(sequential_plt_take(&[2],5,5,10,2).is_err());
  assert!(sequential_plt_take(&[],0,5,10,0).is_err());
  assert!(sequential_plt_take(&[0],0,1,10,0).is_err());
  assert!(sequential_plt_take(&[u64::MAX,1],0,1,10,0).is_err());
 }
 #[tokio::test]
 #[ignore="Requires frozen79MB12bit diagnostic and RIVER_S3_SINGLE_PREFIX=true; local IO correctness only"]
 async fn sequential_plt_real_prefix_matches_independent_reduced_reference() {
  use sha2::{Digest,Sha256};
  assert_eq!(std::env::var("RIVER_S3_SINGLE_PREFIX").as_deref(),Ok("true"));
  let root=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/rgb12-request-m639");
  let gold=image::open(root.join("reference-r2-rgb8.ppm")).unwrap().to_rgb8();
  for (file,hash) in [("source12.jp2","cea7c3a307e1ae7bf35852765e8c952dc41eb2eb296453797d00dbebb13cc733"),
    ("source12-layers10.jp2","832e7eb6c1c2cf013569432512c76fa63f4b977bcfc016b61db5e3594bb971d2")] {
  let path=root.join(file);
  assert_eq!(hex::encode(Sha256::digest(std::fs::read(&path).unwrap())),hash);
  for limit in [0,64*1024*1024] {
   let path=path.clone();let expected=gold.clone();let runtime=tokio::runtime::Handle::current();
   tokio::task::spawn_blocking(move||{
    let cache=Arc::new(RangeCache {limit,state:Mutex::new(CacheState::default())});
    let (reader,opening)=RemoteSource::open_file(&path,runtime,cache).unwrap();
    let mut context=ReadContext::new(None);let payload=reader.acquire_render_header(&mut context).unwrap();
    if reader.header.layers>1 {
     assert!(sequential_full_quality(&reader.header,0));
     assert!(sequential_full_quality(&reader.header,u32::from(reader.header.layers)));
     let mut partial=ReadContext::new(None);
     assert!(reader.prepare_sequential_plt_prefix(&payload,2,1,&mut partial).unwrap().is_none());
     assert_eq!(partial.stats.requests,0,"partial-quality fallback must not fetch full-quality payloads");
     let mut interleaved=reader.header.clone();interleaved.progression=0;interleaved.prefix_compatible=true;
     assert!(!sequential_full_quality(&interleaved,0),"multilayer LRCP is not a resolution prefix");
     interleaved.progression=3;assert!(!sequential_full_quality(&interleaved,0));
     interleaved.progression=1;assert!(sequential_full_quality(&interleaved,0));
    }
    let prepared=reader.prepare_sequential_plt_prefix(&payload,2,0,&mut context).unwrap().expect("multipart PLT prefix");
    assert_eq!(prepared.route,"s3-sequential-plt-prefix");assert_eq!(prepared.view_reduction,2);
    if limit==0 {
     let mut fallback=ReadContext::new(None);
     assert!(reader.prepare_sequential_plt_prefix(&payload,1,0,&mut fallback).unwrap().is_none());
     let before=(fallback.stats.requests,fallback.stats.bytes);
     let reused=reader.prepare_sequential_plt_prefix(&payload,2,0,&mut fallback).unwrap().unwrap();
     assert_eq!(reused.bytes,prepared.bytes);
     assert_eq!((fallback.stats.requests,fallback.stats.bytes),before,"fallback must retain all previously required prefix bytes");
     eprintln!("SEQUENTIAL_PLT_FALLBACK {}",serde_json::json!({"localRequestsBeforeReuse":before.0,"localBytesBeforeReuse":before.1,"additionalReadsForPreviouslyFetchedPrefix":0}));
    }
    let decoded=jpeg2k::Image::from_bytes(&prepared.bytes).unwrap().get_pixels(None).unwrap();
    assert_eq!((decoded.width,decoded.height),expected.dimensions());
    let jpeg2k::ImagePixelData::Rgb16(data)=decoded.data else{panic!("expected12bitRGB normalized to16bit")};
    let actual=data.into_iter().map(|v|((u32::from(v)*255+32767)/65535) as u8).collect::<Vec<_>>();
    assert_eq!(actual,expected.into_raw());
    eprintln!("SEQUENTIAL_PLT_TEST {}",serde_json::json!({"layers":reader.header.layers,"cacheBytes":limit,"localPreparationRequests":context.stats.requests,"localPreparationBytes":context.stats.bytes,"openingRequests":opening.requests,"subsetBytes":prepared.bytes.len(),"pixelsExact":true}));
   }).await.unwrap();
  }
  }
 }
 #[test] fn intermediate_projection_keeps_needed_bands_precincts_and_end_header_markers() {
  let h=Header {image_origin:(0,0),width:4096,height:4096,components:3,bit_depth:8,levels:7,layers:1,
   layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:4096,height:4096,columns:1,rows:1},
   progression:2,is_ht:false,cod:vec![5,2,0,1,1,6,4,4,0,1,0x22,0x33,0x44,0x55,0x66,0x77,0x88],
   first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:true,payload_digest:[0;32]};
  let mut bytes=vec![];
  for (code,value) in [(0xff52u16,h.cod.clone()),(0xff5c,vec![0x40;20])] {
   bytes.extend(code.to_be_bytes());bytes.extend(((value.len()+2) as u16).to_be_bytes());bytes.extend(value);
  }
  let before=bytes.clone();let projected=lowest_header_segments(&bytes,&h,2,true).unwrap().unwrap();
  assert_eq!(bytes,before);let n=usize::from(be16(&projected,2).unwrap());let cod=&projected[4..n+2];
  assert_eq!(cod.len(),15);assert_eq!(cod[0],5);assert_eq!(cod[5],4);assert_eq!(&cod[10..],&h.cod[10..15]);
  let at=n+2;assert_eq!(be16(&projected,at).unwrap(),0xff5c);assert_eq!(be16(&projected,at+2).unwrap(),16);
 }
 #[test] fn projection_preserves_source_and_component_quantization() {
  let h=Header {image_origin:(0,0),width:4096,height:4096,components:3,bit_depth:8,levels:7,layers:1,
   layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:4096,height:4096,columns:1,rows:1},progression:2,is_ht:false,
   cod:vec![1,2,0,1,0,6,4,4,0,1,0x22,0x33,0x44,0x55,0x66,0x77,0x88],first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:true,payload_digest:[0;32]};
  let mut siz=vec![0,0];for n in [4096u32,4096,0,0,4096,4096,0,0] {siz.extend(n.to_be_bytes());}siz.extend([0,3,7,1,1,7,1,1,7,1,1]);
  let coc=[vec![0,1],h.cod[5..].to_vec()].concat();let mut main=vec![0xff,0x4f];
  for (code,bytes) in [(0xff51,siz),(0xff52,h.cod.clone()),(0xff5c,vec![0x40;20]),(0xff53,coc),(0xff5d,vec![1,0x40,0x40,0x48])] {
   main.extend((code as u16).to_be_bytes());main.extend(((bytes.len()+2) as u16).to_be_bytes());main.extend(bytes);
  }
  let original=main.clone();let projected=lowest_header(&main,&h,6).unwrap().unwrap();assert_eq!(main,original);
  let mut at=2;let mut found=0;
  while at<projected.len() {
   let code=be16(&projected,at).unwrap();let length=usize::from(be16(&projected,at+2).unwrap());let value=&projected[at+4..at+length+2];
   match code {
    0xff51=>assert_eq!((be32(value,2).unwrap(),be32(value,6).unwrap()),(64,64)),
    0xff52=>assert_eq!((value.len(),value[5],value[6],value[7],value[10]),(11,0,4,4,0x22)),
    0xff53=>assert_eq!((value.len(),value[2],value[3],value[4],value[7]),(8,0,4,4,0x22)),
    0xff5c=>assert_eq!(value,&[0x40,0x40]),
    0xff5d=>assert_eq!(value,&[1,0x40,0x40]),_=>panic!("unexpected marker")
   }found+=1;at+=length+2;
  }
  assert_eq!(found,5);
 }
}
