//! Stream a large single-tile PLT into sparse spatial demand. No full packet
//! descriptor/length array is allocated; original entropy remains unchanged.
use super::*;

struct Grid {resolution:u8, first:usize, end:usize, columns:u64, width:u64, height:u64}
fn grids(h:&Header)->Result<Vec<Grid>> {
 let mut grids=vec![];let mut first=0usize;
 for resolution in 0..h.levels {
  let scale=1u64<<(h.levels-1-resolution);let pp=h.cod.get(10+usize::from(resolution)).copied().unwrap_or(0xff);
  let width=(1u64<<(pp&15))*scale;let height=(1u64<<(pp>>4))*scale;
  let columns=u64::from(h.width).div_ceil(width);let rows=u64::from(h.height).div_ceil(height);
  let count=columns.checked_mul(rows).and_then(|n|n.checked_mul(u64::from(h.components))).context("streamed ROI geometry overflow")?;
  let end=first.checked_add(usize::try_from(count)?).context("streamed ROI packet count overflow")?;
  grids.push(Grid {resolution,first,end,columns,width,height});first=end;
 }
 Ok(grids)
}

fn rebuilt_plt(selected:&[(usize,Range<u64>)],total:usize,fill:u8)->Result<Vec<u8>> {
 let mut result=vec![];let mut chunk=Vec::with_capacity(65532);let mut sequence=0u16;
 let mut emit=|byte:u8|->Result<()> {
  chunk.push(byte);
  if chunk.len()==65532 {
   ensure!(sequence<256,"rebuilt ROI PLT has too many segments");
   result.extend([0xff,0x58]);result.extend((chunk.len() as u16+3).to_be_bytes());result.push(sequence as u8);result.append(&mut chunk);sequence+=1;
  }
  Ok(())
 };
 let mut previous=0;
 for (ordinal,range) in selected {
  for _ in previous..*ordinal {emit(fill)?;}
  let mut n=u32::try_from(range.end-range.start)?;let mut bytes=[0u8;5];let mut at=4;bytes[at]=(n&127) as u8;n>>=7;
  while n>0 {at-=1;bytes[at]=((n&127) as u8)|128;n>>=7;}
  for &byte in &bytes[at..] {emit(byte)?;}
  previous=ordinal+1;
 }
 for _ in previous..total {emit(fill)?;}
 drop(emit);
 if !chunk.is_empty() {
  ensure!(sequence<256,"rebuilt ROI PLT has too many segments");
  result.extend([0xff,0x58]);result.extend((chunk.len() as u16+3).to_be_bytes());result.push(sequence as u8);result.extend(chunk);
 }
 Ok(result)
}

impl RemoteSource {
 pub(super) fn prepare_streamed_single_roi(&self,payload:&HeaderPayload,reduction:u8,layers:u32,roi:&precinct_roi::Selection,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
  let h=&self.header;
  if !std::env::var("RIVER_S3_STREAMED_PLT_ROI").is_ok_and(|v|v=="true" || v=="1") {return Ok(None);}
  // Existing ordinary-size plans retain their cache and rebasing behavior.
  if !h.packet_compatible || h.is_ht || h.layers!=1 || layers>1 || h.progression!=2 || h.cod[9]!=1
   || h.cod[0]&!5!=0 || h.image_origin!=(0,0) || h.layout.origin_x!=0 || h.layout.origin_y!=0
   || h.layout.columns!=1 || h.layout.rows!=1 {return Ok(None);}
  let grids=grids(h)?;let total=grids.last().context("missing streamed ROI geometry")?.end;
  if total<=MAX_PACKETS || total>8*1024*1024 {return Ok(None);}
  let fill=1+usize::from(h.cod[0]&4!=0)*2;
  ensure!(total.checked_mul(fill).is_some_and(|n|n<=context.limit as usize),"streamed ROI empty packets exceed memory budget");
  let part=self.part_at(h.first_part,context)?;
  if part.tile!=0 || part.ordinal!=0 || part.total>1 {return Ok(None);}
  let mut sot=self.read(part.offset..part.offset+12,context)?;
  let mut at=part.offset+12;let mut slab_end=0;let mut headers=vec![];
  let slab_bytes=4*1024*1024u64;let mut expected=0u16;
  let (mut count,mut sum,mut value,mut grid_index)=(0usize,0u64,0u64,0usize);let mut pending=false;
  let mut selected:Vec<(usize,Range<u64>)>=vec![];
  loop {
   context.check()?;
   ensure!(at-part.offset<=16*1024*1024,"streamed ROI tile header exceeds limit");
   let head=self.read(at..at+4,context)?;let marker=be16(&head,0)?;
   if marker==0xff93 {at+=2;break;}
   let length=u64::from(be16(&head,2)?);
   ensure!(length>=2 && at+length+2<=part.offset+part.length,"invalid streamed ROI marker extent");
   if marker==0xff64 {at+=length+2;continue;}
   if marker==0xff58 {
    if at+length+2>slab_end {
     slab_end=(at/self.block_size*self.block_size+slab_bytes).min(part.offset+part.length);
     let leases=self.prefetch_ranges(&[(true,at..slab_end)],context,usize::MAX)?;
     context.pinned.clear();context.pin_packet_blocks(leases);
    }
    let bytes=self.read(at+4..at+length+2,context)?;
    ensure!(expected<256 && bytes.first().copied()==Some(expected as u8),"invalid streamed ROI PLT sequence");expected+=1;
    for &byte in &bytes[1..] {
     ensure!(value<=u64::from(u32::MAX)>>7,"streamed ROI PLT length overflow");
     value=(value<<7)|u64::from(byte&127);pending=byte&128!=0;
     if !pending {
      ensure!(value>0 && count<total,"streamed ROI PLT count exceeds geometry");
      while count>=grids[grid_index].end {grid_index+=1;}
      let grid=&grids[grid_index];let cell=(count-grid.first)/usize::from(h.components);
      let packet=Packet {resolution:grid.resolution,layer:0,component:((count-grid.first)%usize::from(h.components)) as u8,
       x:(cell as u64%grid.columns)*grid.width,y:(cell as u64/grid.columns)*grid.height};
      let end=sum.checked_add(value).context("streamed ROI PLT payload overflow")?;
      if packet.resolution<h.levels-reduction && roi.keeps(h,&packet) {
       ensure!(selected.len()<65536 && context.admit_metadata(std::mem::size_of::<(usize,Range<u64>)>()),"streamed ROI selection exceeds metadata budget");
       selected.push((count,sum..end));
      }
      sum=end;count+=1;value=0;
     }
    }
   }else {
    if matches!(marker,0xff52|0xff5f|0xff61) {return Ok(None);}
    let bytes=self.read(at..at+length+2,context)?;
    if marker==0xff53 && !identical_coc(&bytes[4..],h.components,&h.cod) {return Ok(None);}
    headers.extend(bytes);ensure!(headers.len()<=MAX_HEADER,"streamed ROI coding header exceeds limit");
   }
   at+=length+2;
  }
  if expected==0 || (part.total==0 && count<total && !pending) {return Ok(None);}
  ensure!(!pending && count==total && sum==part.offset+part.length-at,"streamed ROI PLT does not cover geometry and payload");
  let payload_bytes=selected.iter().try_fold(0usize,|n,(_,r)|n.checked_add(usize::try_from(r.end-r.start).ok()?)).context("streamed ROI selected size overflow")?;
  let capacity=total.checked_sub(selected.len()).and_then(|n|n.checked_mul(fill)).and_then(|n|n.checked_add(payload_bytes)).context("streamed ROI reconstruction overflow")?;
  ensure!(capacity<=context.limit as usize,"streamed ROI reconstruction exceeds memory budget");
  let spans=selected.iter().map(|(_,r)|(true,at+r.start..at+r.end)).collect::<Vec<_>>();
  let bytes=self.selected_payload(&spans,context)?;
  // Keep an index for the reconstructed packets too. Otherwise decoders must
  // parse millions of empty packet headers to reach the tiny spatial demand.
  headers.extend(rebuilt_plt(&selected,total,fill as u8)?);
  ensure!(capacity.saturating_add(headers.len()).saturating_add(payload.main.len())<=context.limit as usize,"indexed ROI reconstruction exceeds memory budget");
  let mut body=Vec::with_capacity(capacity);let (mut previous,mut cursor)=(0,0);
  for (ordinal,range) in selected {
   context.check()?;
   for n in previous..ordinal {append_empty_packet(&mut body,h.cod[0],n);}
   let n=usize::try_from(range.end-range.start)?;body.extend_from_slice(&bytes[cursor..cursor+n]);cursor+=n;previous=ordinal+1;
  }
  for n in previous..total {append_empty_packet(&mut body,h.cod[0],n);}
  sot[6..10].copy_from_slice(&u32::try_from(12+headers.len()+2+body.len())?.to_be_bytes());sot[10]=0;sot[11]=1;
  let mut main=payload.main.clone();main.extend(sot);main.extend(headers);main.extend([0xff,0x93]);main.extend(body);main.extend([0xff,0xd9]);
  let jp2=!payload.container.is_empty();
  let bytes=if jp2 {let mut container=payload.container.clone();container.extend(u32::try_from(main.len()+8)?.to_be_bytes());container.extend(b"jp2c");container.extend(main);container}else{main};
  Ok(Some(PreparedSubset {view_reduction:0,technical_origin:None,bytes,jp2,route:"s3-streamed-plt-roi",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0}))
 }
}

#[cfg(test)] mod tests {
 use super::*;
 #[test] fn rebuilt_plt_preserves_variable_lengths_and_sequence_across_segments() {
  let selected=vec![(65531,0..16384),(65534,16384..16385)];
  let bytes=rebuilt_plt(&selected,65538,3).unwrap();let mut at=0;let mut expected=0u8;let mut encoded=vec![];
  while at<bytes.len() {
   assert_eq!(be16(&bytes,at).unwrap(),0xff58);let n=usize::from(be16(&bytes,at+2).unwrap());
   assert_eq!(bytes[at+4],expected);expected+=1;encoded.extend_from_slice(&bytes[at+5..at+n+2]);at+=n+2;
  }
  let lengths=parse_plt(&encoded).unwrap();assert_eq!(lengths.len(),65538);
  assert_eq!(lengths[65531],16384);assert_eq!(lengths[65534],1);
  assert!(lengths.iter().enumerate().all(|(i,&n)|i==65531 || i==65534 || n==3));
  assert_eq!(expected,2);
 }
}
