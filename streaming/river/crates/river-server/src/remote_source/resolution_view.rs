//! Entropy-preserving reduced-resolution headers for complete tiled previews.
use super::*;
impl RemoteSource {
 pub(super) fn prepare_reduced_prefix_view(&self,parts:&[TilePart],payload:&HeaderPayload,reduction:u8,whole:bool,complete_grid:bool,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let h=&self.header;let native=u64::from(h.layout.columns)*u64::from(h.layout.rows);
  if !whole || complete_grid || reduction==0 || h.layers!=1 || !h.prefix_compatible || h.is_ht
   || h.cod[9]!=1 || h.cod[8]!=0 || h.cod[0]&6!=0 || h.image_origin!=(0,0)
   || h.layout.origin_x!=0 || h.layout.origin_y!=0 || !(1..=16384).contains(&native)
   || parts.len() as u64!=native || (native>1 && std::env::var("RIVER_S3_NATIVE_TILES").is_ok_and(|v|v=="true" || v=="1")) {return Ok(None);}
  #[cfg(all(feature="grok",unix))] if context.pipeline.is_some() {return Ok(None);}
  let scale=1u32<<reduction;
  if h.layout.width%scale!=0 || h.layout.height%scale!=0 {return Ok(None);}
  if parts.iter().any(|p|p.ordinal!=0 || p.total!=1)
   || parts.iter().map(|p|p.tile).collect::<HashSet<_>>().len()!=parts.len() {return Ok(None);}
  let Some(mut main)=project(&payload.main[2..],h,reduction,true)? else{return Ok(None);};
  main.splice(..0,[0xff,0x4f]);
  let wanted=h.levels-1-reduction;
  let mut plans=vec![];let mut budget=0usize;
  for part in parts {
   let (header,start,lengths,compatible)=self.tile_header(part,context)?;
   if !compatible {return Ok(None);}
   let prefix=if let Some(lengths)=lengths {
    let (count,total)=multipart_prefix::counts(h,part.tile,wanted)?;
    ensure!(lengths.len()==total && lengths.iter().all(|&n|n>0),"invalid reduced-view PLT packet count or length");
    let sum=lengths.iter().try_fold(0u64,|a,&b|a.checked_add(b)).context("reduced-view PLT overflow")?;
    ensure!(start.checked_add(sum)==part.offset.checked_add(part.length),"reduced-view PLT does not cover tile payload");
    Some(lengths[..count].iter().try_fold(0u64,|a,&b|a.checked_add(b)).context("reduced-view prefix overflow")?)
   } else {None};
   let Some(header)=project(&header,h,reduction,false)? else{return Ok(None);};
   budget=budget.saturating_add(header.len()+160);
   if budget>context.metadata_limit.saturating_sub(context.metadata_bytes) {return Ok(None);}
   plans.push((part,header,start,prefix));
  }
  if !context.admit_metadata(budget) {return Ok(None);}
  for (part,header,start,prefix) in plans {
   let bytes=if let Some(length)=prefix {self.selected_payload(&[(true,start..start+length)],context)?} else {
    let (packets,total)=self.ordered_packets(part.tile,Some(wanted),context)?;
    let Some(mut bytes)=self.prefix_payload(part,start,&packets,total,reduction,None,context)? else{return Ok(None);};
    let omitted=total.checked_sub(packets.len()).context("invalid projected packet count")?;
    ensure!(bytes.len()>=omitted,"short projected resolution prefix");bytes.truncate(bytes.len()-omitted);bytes
   };
   let mut sot=self.read(part.offset..part.offset+12,context)?;
   sot[6..10].copy_from_slice(&u32::try_from(14+header.len()+bytes.len())?.to_be_bytes());
   main.extend(sot);main.extend(header);main.extend([0xff,0x93]);main.extend(bytes);
   ensure!(main.len()<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"projected tiled view exceeds request memory budget");
  }
  main.extend([0xff,0xd9]);let jp2=!payload.container.is_empty();
  let bytes=if jp2 {let mut container=payload.container.clone();resize_jp2(&mut container,h.width.div_ceil(scale),h.height.div_ceil(scale))?;container.extend(u32::try_from(main.len()+8)?.to_be_bytes());container.extend(b"jp2c");container.extend(main);container}else{main};
  Ok(Some(PreparedSubset {view_reduction:reduction,technical_origin:None,bytes,jp2,route:"s3-reduced-resolution-view",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0}))
 }
}
pub(super) fn project(bytes:&[u8],h:&Header,reduction:u8,main:bool)->Result<Option<Vec<u8>>> {
 let retained=h.levels-1-reduction;let scale=1u32<<reduction;let mut at=0;let mut out=vec![];
 while at<bytes.len() {
  let marker=be16(bytes,at)?;let length=usize::from(be16(bytes,at+2)?);
  ensure!(length>=2 && at+length+2<=bytes.len(),"invalid reduced tiled header");
  let mut v=bytes[at+4..at+length+2].to_vec();
  match marker {
   0xff51=>{
    if !main || v.len()<36 {return Ok(None);}
    v[..2].copy_from_slice(&[0,0]);
    for (i,n) in [h.width.div_ceil(scale),h.height.div_ceil(scale),0,0,h.layout.width/scale,h.layout.height/scale,0,0].into_iter().enumerate() {v[2+i*4..6+i*4].copy_from_slice(&n.to_be_bytes());}
   },
   0xff52=>{if !main || v!=h.cod {return Ok(None);}v[5]=retained;v.truncate(if v[0]&1!=0 {11+usize::from(retained)}else{10});},
   0xff53=>{if !identical_coc(&v,h.components,&h.cod) {return Ok(None);}v[2]=retained;v.truncate(if v[1]&1!=0 {8+usize::from(retained)}else{7});},
   0xff5c=>{let n=2+3*usize::from(retained);if v.len()<n || v[0]&31!=0 {return Ok(None);}v.truncate(n);},
   0xff5d=>{let n=3+3*usize::from(retained);if v.len()<n || v[1]&31!=0 {return Ok(None);}v.truncate(n);},
   0xff55|0xff57|0xff64=>{at+=length+2;continue;},
   _=>return Ok(None),
  }
  out.extend(marker.to_be_bytes());out.extend(u16::try_from(v.len()+2)?.to_be_bytes());out.extend(v);at+=length+2;
 }
 Ok(Some(out))
}

#[cfg(test)] mod tests {
 use super::*;
 #[test] fn reduced_headers_preserve_tile_grid_and_retained_quantization() {
  for style in 0..64 {
  let h=Header {image_origin:(0,0),width:4091,height:3079,components:3,display_rgb_only:false,bit_depth:16,levels:7,layers:1,
   layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:1024,height:1024,columns:4,rows:4},progression:2,is_ht:false,
   cod:vec![1,2,0,1,1,6,4,4,style,1,0x44,0x44,0x44,0x44,0x44,0x44,0x44],first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:true,payload_digest:[0;32]};
  let mut siz=vec![0,0];for n in [h.width,h.height,0,0,1024,1024,0,0] {siz.extend(n.to_be_bytes());}siz.extend([0,3,15,1,1,15,1,1,15,1,1]);
  let mut main=vec![];
  for (code,v) in [(0xff51u16,siz),(0xff52,h.cod.clone()),(0xff5c,vec![0x40;20])] {main.extend(code.to_be_bytes());main.extend(((v.len()+2) as u16).to_be_bytes());main.extend(v);}
  for reduction in 1..h.levels {
  let scale=1u32<<reduction;let retained=h.levels-1-reduction;
  let before=main.clone();let view=project(&main,&h,reduction,true).unwrap().unwrap();assert_eq!(before,main);
  let mut at=0;
  while at<view.len() {let code=be16(&view,at).unwrap();let n=usize::from(be16(&view,at+2).unwrap());let v=&view[at+4..at+n+2];match code {
   0xff51=>{assert_eq!((be32(v,2).unwrap(),be32(v,6).unwrap()),(h.width.div_ceil(scale),h.height.div_ceil(scale)));assert_eq!((be32(v,18).unwrap(),be32(v,22).unwrap()),(1024/scale,1024/scale));},
   0xff52=>{assert_eq!((v[5],v.len()),(retained,11+usize::from(retained)));assert_eq!(v[8],style);},0xff5c=>assert_eq!(v,vec![0x40;2+3*usize::from(retained)]),_=>panic!("unexpected projected marker")};at+=n+2;}
  }
  }
 }
}
