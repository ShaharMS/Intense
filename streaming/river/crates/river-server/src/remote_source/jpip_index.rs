//! Bounded optional Part9 metadata parsing and advertised range reads.
use anyhow::{Result,ensure,Context};
use super::{be16,be32,be64};
use std::ops::Range;
const MAX_BYTES:usize=4*1024*1024;
const MAX_ENTRIES:usize=100_000;
#[derive(Debug,Clone,PartialEq,Eq)]
pub(super) struct Entry {pub tile:u16,pub ordinal:u8,pub offset:u64,pub length:u32}
#[derive(Clone,Copy,Debug)]
pub(super) enum Locator {Root {offset:u64,length:u64},Codestream {offset:u64,length:u64}}

fn box_header(bytes:&[u8],at:usize)->Result<(u64,usize,[u8;4])> {
 let n=be32(bytes,at)?;ensure!(n!=0,"unbounded advertised JPIP box");
 let kind=bytes.get(at+4..at+8).context("short advertised JPIP box")?.try_into()?;
 let (length,header)=if n==1 {(be64(bytes,at+8)?,16)}else{(u64::from(n),8)};
 ensure!(length>=header as u64,"invalid advertised JPIP box length");Ok((length,header,kind))
}
fn window_read(window:&[u8],base:u64,range:Range<u64>,read:&mut impl FnMut(Range<u64>)->Result<Vec<u8>>)->Result<Vec<u8>> {
 ensure!(range.start>=base && range.end>=range.start,"invalid JPIP metadata window range");
 let window_end=base+window.len() as u64;let mut cursor=range.start;let mut bytes=Vec::new();
 if cursor<window_end {
  let end=range.end.min(window_end);bytes.extend_from_slice(&window[usize::try_from(cursor-base)?..usize::try_from(end-base)?]);cursor=end;
 }
 if cursor<range.end {let tail=read(cursor..range.end)?;ensure!(tail.len() as u64==range.end-cursor,"truncated JPIP metadata range");bytes.extend(tail);}
 Ok(bytes)
}
fn required_cidx(offset:u64,length:u64,source_size:u64,mut read:impl FnMut(Range<u64>)->Result<Vec<u8>>)->Result<Vec<u8>> {
 let end=offset.checked_add(length).context("JPIP index container overflow")?;
 ensure!(length>=8 && end<=source_size,"JPIP index container outside source");
 // A modest first window normally contains cptr and the complete tpix table.
 // Declared CIDX size is not a read size: packet-index children can be huge.
 let prefix_end=end.min(offset.saturating_add(16*1024));let window=read(offset..prefix_end)?;
 ensure!(window.len() as u64==prefix_end-offset,"truncated JPIP index prefix");
 let (actual,header,kind)=box_header(&window,0)?;ensure!(actual==length && &kind==b"cidx","JPIP advertised index disagrees with source");
 let mut cursor=offset+header as u64;let mut used=vec![];let (mut pointer,mut parts)=(false,false);
 for _ in 0..4096 {
  ensure!(cursor<end,"JPIP required index children missing");
  let bytes=window_read(&window,offset,cursor..end.min(cursor.saturating_add(16)),&mut read)?;
  let (length,header,kind)=box_header(&bytes,0)?;let child_end=cursor.checked_add(length).context("JPIP child extent overflow")?;
  ensure!(child_end<=end,"JPIP child outside index container");
  if &kind==b"cptr" || &kind==b"tpix" {
   if &kind==b"cptr" {ensure!(!pointer && length==header as u64+20,"invalid or duplicate JPIP codestream pointer");pointer=true;}
   else {ensure!(!parts,"duplicate JPIP tile-part index");parts=true;}
   ensure!(length<=(MAX_BYTES-used.len()) as u64,"required JPIP metadata exceeds limit");
   used.extend(window_read(&window,offset,cursor..child_end,&mut read)?);
   if pointer && parts {return Ok(used);}
  }
  cursor=child_end;
 }
 anyhow::bail!("too many JPIP index children")
}
impl super::RemoteSource {
 fn jpip_box(&self,offset:u64,length:u64,kind:&[u8;4],limit:usize,context:&mut super::ReadContext)->Result<Vec<u8>> {
  ensure!((8..=limit as u64).contains(&length) && length<=context.limit.saturating_sub(context.stats.bytes),"advertised JPIP metadata exceeds available budget");
  let end=offset.checked_add(length).context("JPIP advertised extent overflow")?;ensure!(end<=self.meta.size,"JPIP advertised box outside source");
  let bytes=self.read(offset..end,context)?;let (actual,header,found)=box_header(&bytes,0)?;
  ensure!(actual==length && &found==kind,"JPIP advertised box disagrees with source");Ok(bytes[header..].to_vec())
 }
 pub(super) fn read_jpip_index(&self,locator:Locator,start:u64,end:u64,first_part:u64,tiles:u64,context:&mut super::ReadContext)->Result<Vec<Entry>> {
  let (offset,length)=match locator {
   Locator::Codestream {offset,length}=>(offset,length),
   Locator::Root {offset,length}=>{
    let root=self.jpip_box(offset,length,b"fidx",65536,context)?;
    let proxy=child(&root,b"prxy")?.context("JPIP root proxy missing")?;
    let original=be64(proxy,0)?;let (source_length,source_header,source_kind)=box_header(proxy,8)?;
    ensure!(&source_kind==b"jp2c" && original.checked_add(source_header as u64)==Some(start) && original.checked_add(source_length)==Some(end),"JPIP proxy disagrees with source codestream");
    let mut at=8+source_header;ensure!(proxy.get(at)==Some(&1),"unsupported multiple JPIP proxy indexes");at+=1;
    let offset=be64(proxy,at)?;at+=8;let (length,header,kind)=box_header(proxy,at)?;
    ensure!(&kind==b"cidx" && at+header==proxy.len(),"invalid JPIP codestream index proxy");(offset,length)
   }
  };
  let required=required_cidx(offset,length,self.meta.size,|range|{
   context.check()?;ensure!(range.end-range.start<=context.limit.saturating_sub(context.stats.bytes),"required JPIP metadata exceeds available budget");self.read(range,context)
  })?;
  parse(&required,start,end,first_part,tiles)
 }
 pub(super) fn jpip_selected_parts(&self,entries:&[Entry],selected:&std::collections::HashSet<u16>,selected_prefix:u64,complete_parts:bool,context:&mut super::ReadContext)->Result<Vec<super::TilePart>> {
  let candidates=entries.iter().enumerate().filter(|(_,e)|selected.contains(&e.tile)).collect::<Vec<_>>();
  for page in candidates.chunks(256) {
   let whole=page.iter().map(|(_,e)|(true,e.offset..e.offset+u64::from(e.length))).collect::<Vec<_>>();
   let fuse=complete_parts && context.can_pin_ranges(self.block_size,&whole);
   let spans=if fuse {whole}else{page.iter().map(|(_,e)|(true,e.offset..(e.offset+selected_prefix.max(12)).min(e.offset+u64::from(e.length)))).collect::<Vec<_>>()};
   let leases=self.prefetch_ranges(&spans,context,usize::MAX)?;context.pin_packet_blocks(leases);
  }
  let mut parts=Vec::with_capacity(candidates.len());
  for (position,entry) in candidates {
   let part=self.part_at(entry.offset,context)?;
   ensure!(part.tile==entry.tile && part.ordinal==entry.ordinal && part.length==u64::from(entry.length),"JPIP tile-part entry disagrees with source SOT");
   super::read_trace::index_entry_from(context.trace_id,&self.id,position,part.tile,part.offset..part.offset+part.length,self.block_size,"validatedJpipSot");parts.push(part);
  }
  Ok(parts)
 }
}

fn child<'a>(bytes:&'a [u8],wanted:&[u8;4])->Result<Option<&'a [u8]>> {
 ensure!(bytes.len()<=MAX_BYTES,"JPIP index metadata exceeds limit");
 let (mut at,mut count,mut found)=(0usize,0usize,None);
 while at<bytes.len() {
  count+=1;ensure!(count<=4096,"too many JPIP index boxes");
  let n=be32(bytes,at)?;let header=if n==1 {16}else{8};
  let length=match n {0=>bytes.len()-at,1=>usize::try_from(be64(bytes,at+8)?)?,_=>n as usize};
  let end=at.checked_add(length).context("JPIP box extent overflow")?;
  ensure!(length>=header && end<=bytes.len(),"invalid JPIP box extent");
  if bytes.get(at+4..at+8)==Some(wanted.as_slice()) {
   ensure!(found.is_none(),"duplicate JPIP index box");found=Some(&bytes[at+header..end]);
  }
  at=end;
 }
 Ok(found)
}
fn integer(bytes:&[u8],at:&mut usize,width:usize)->Result<u64> {
 let value=if width==8 {be64(bytes,*at)?}else{u64::from(be32(bytes,*at)?)};*at+=width;Ok(value)
}
pub(super) fn parse(bytes:&[u8],start:u64,end:u64,first_part:u64,tiles:u64)->Result<Vec<Entry>> {
 ensure!(start<first_part && first_part<end && (1..=65536).contains(&tiles),"invalid JPIP reference geometry");
 let pointer=child(bytes,b"cptr")?.context("JPIP codestream pointer missing")?;
 ensure!(pointer.len()==20 && be16(pointer,0)?==0 && be16(pointer,2)?==0,"unsupported external or fragmented JPIP codestream");
 ensure!(be64(pointer,4)?==start && be64(pointer,12)?==end-start,"JPIP codestream pointer disagrees with source");
 let table=child(bytes,b"tpix")?.context("JPIP tile-part table missing")?;
 let faix=child(table,b"faix")?.context("JPIP fragment array missing")?;
 let version=*faix.first().context("empty JPIP fragment array")?;ensure!(version<=3,"unsupported JPIP fragment array version");
 let width=if version&1!=0 {8}else{4};let mut at=1;
 let columns=integer(faix,&mut at,width)?;let rows=integer(faix,&mut at,width)?;
 ensure!((1..=256).contains(&columns) && rows==tiles,"JPIP fragment array geometry disagrees with source");
 let count=usize::try_from(columns.checked_mul(rows).context("JPIP fragment count overflow")?)?;
 ensure!(count<=MAX_ENTRIES,"JPIP fragment array exceeds entry limit");
 let stride=width*2+if version&2!=0 {4}else{0};
 ensure!(count.checked_mul(stride).and_then(|n|n.checked_add(at))==Some(faix.len()),"truncated or oversized JPIP fragment array");
 let mut entries=Vec::with_capacity(count);
 for tile in 0..rows {
  let mut padding=false;
  for ordinal in 0..columns {
   let relative=integer(faix,&mut at,width)?;let length=integer(faix,&mut at,width)?;
   let aux=if version&2!=0 {let n=be32(faix,at)?;at+=4;n}else{0};
   if length==0 {ensure!(relative==0 && aux==0,"invalid JPIP fragment padding");padding=true;continue;}
   ensure!(!padding && length>=14,"invalid JPIP tile-part extent or ordinal");
   let offset=start.checked_add(relative).context("JPIP fragment offset overflow")?;
   ensure!(offset>=first_part && offset.checked_add(length).is_some_and(|n|n<=end-2),"JPIP fragment lies outside tile payload");
   entries.push(Entry {tile:u16::try_from(tile)?,ordinal:ordinal as u8,offset,length:u32::try_from(length).context("JPIP tile-part length exceeds supported extent")?});
  }
 }
 entries.sort_unstable_by_key(|e|e.offset);let mut cursor=first_part;
 for entry in &entries {ensure!(entry.offset==cursor,"JPIP tile-part index is incomplete or overlapping");cursor=cursor.checked_add(u64::from(entry.length)).context("JPIP tile-part extent overflow")?;}
 ensure!(cursor==end-2,"JPIP tile-part index does not cover codestream");
 Ok(entries)
}

#[cfg(test)]mod tests {
 use super::*;
 fn boxed(kind:&[u8;4],data:&[u8])->Vec<u8> {let mut b=((data.len()+8) as u32).to_be_bytes().to_vec();b.extend(kind);b.extend(data);b}
 fn fixture(version:u8)->Vec<u8> {
  let mut pointer=vec![0;4];pointer.extend(100u64.to_be_bytes());pointer.extend(54u64.to_be_bytes());
  let width=if version&1!=0 {8}else{4};let mut table=vec![version];
  fn write(table:&mut Vec<u8>,n:u64,width:usize) {if width==8 {table.extend(n.to_be_bytes());}else{table.extend((n as u32).to_be_bytes());}}
  write(&mut table,2,width);write(&mut table,2,width);
  for (offset,length) in [(10,14),(24,14),(38,14),(0,0)] {
   write(&mut table,offset,width);write(&mut table,length,width);
   if version&2!=0 {table.extend(0u32.to_be_bytes());}
  }
  let mut cidx=boxed(b"cptr",&pointer);cidx.extend(boxed(b"tpix",&boxed(b"faix",&table)));cidx
 }
 #[test]fn bounded_versions_offsets_padding_and_complete_coverage() {
  for version in 0..4 {let entries=parse(&fixture(version),100,154,110,2).unwrap();assert_eq!(entries.iter().map(|e|(e.tile,e.ordinal,e.offset,e.length)).collect::<Vec<_>>(),vec![(0,0,110,14),(0,1,124,14),(1,0,138,14)]);}
 }
 #[test]fn rejects_truncation_pointer_mismatch_and_wrong_geometry() {
  let bytes=fixture(2);for n in 0..bytes.len() {assert!(parse(&bytes[..n],100,154,110,2).is_err());}
  assert!(parse(&bytes,101,154,110,2).is_err());assert!(parse(&bytes,100,154,110,3).is_err());
  let mut b=bytes.clone();b[8]=1;assert!(parse(&b,100,154,110,2).is_err());
  let mut b=bytes.clone();b[44]=4;assert!(parse(&b,100,154,110,2).is_err());
 }
 #[test]fn rejects_overlaps_gaps_offset_overflow_and_unbounded_tables() {
  let original=fixture(2);
  for (at,value) in [(53,0u32),(65,10),(77,39),(57,13)] {
   let mut b=original.clone();b[at..at+4].copy_from_slice(&value.to_be_bytes());assert!(parse(&b,100,154,110,2).is_err());
  }
  let mut b=fixture(3);b[61..69].copy_from_slice(&u64::MAX.to_be_bytes());assert!(parse(&b,100,154,110,2).is_err());
  let mut b=original.clone();b[45..49].copy_from_slice(&256u32.to_be_bytes());b[49..53].copy_from_slice(&65536u32.to_be_bytes());
  assert!(parse(&b,100,154,110,65536).unwrap_err().to_string().contains("entry limit"));
  assert!(parse(&vec![0;MAX_BYTES+1],100,154,110,2).is_err());
 }
 #[test]fn supports_extended_boxes_but_rejects_duplicate_pointers() {
  let original=fixture(0);let mut extended=1u32.to_be_bytes().to_vec();extended.extend(b"cptr");extended.extend(36u64.to_be_bytes());extended.extend(&original[8..28]);extended.extend(&original[28..]);
  assert_eq!(parse(&extended,100,154,110,2).unwrap(),parse(&original,100,154,110,2).unwrap());
  let mut duplicate=original.clone();duplicate.extend(&original[..28]);assert!(parse(&duplicate,100,154,110,2).is_err());
 }
 #[test]fn reads_required_tables_without_consuming_large_unused_children() {
  let original=fixture(2);let gap=8*1024*1024usize;
  for unused_first in [false,true] {
   let mut file=vec![0;8];file.extend(&original[..28]);
   if !unused_first {file.extend(&original[28..]);}
   file.extend((gap as u32).to_be_bytes());file.extend(b"free");file.resize(file.len()+gap-8,0);
   if unused_first {file.extend(&original[28..]);}
   let length=file.len() as u32;file[..4].copy_from_slice(&length.to_be_bytes());file[4..8].copy_from_slice(b"cidx");
   let mut reads=vec![];
   let required=required_cidx(0,file.len() as u64,file.len() as u64,|range|{reads.push(range.clone());Ok(file[range.start as usize..range.end as usize].to_vec())}).unwrap();
   assert_eq!(parse(&required,100,154,110,2).unwrap().len(),3);
   assert!(reads.iter().map(|r|r.end-r.start).sum::<u64>()<32768);
   assert!(reads.iter().all(|r|!r.contains(&(gap as u64/2))),"unused payload must be skipped");
   if !unused_first {assert_eq!(reads.len(),1,"stop once both required children are available");}
  }
 }
 #[test]fn oversized_required_child_is_rejected_before_body_fetch() {
  let original=fixture(2);let length=8*1024*1024u32;
  let mut prefix=length.to_be_bytes().to_vec();prefix.extend(b"cidx");prefix.extend(&original[..28]);prefix.extend((length-36).to_be_bytes());prefix.extend(b"tpix");prefix.resize(16384,0);
  let mut calls=0;let error=required_cidx(0,u64::from(length),u64::from(length),|range|{calls+=1;assert_eq!(range,0..16384);Ok(prefix.clone())}).unwrap_err();
  assert!(error.to_string().contains("metadata exceeds limit"));assert_eq!(calls,1);
 }
 #[test]fn independent_openjpeg15_index_matches_every_source_sot() {
  let file=include_bytes!("../../tests/fixtures/indexed-cidx/reference.jp2");
  let stream=child(file,b"jp2c").unwrap().unwrap();let start=(stream.as_ptr() as usize-file.as_ptr() as usize) as u64;
  let mut first=2usize;while be16(stream,first).unwrap()!=0xff90 {first+=2+usize::from(be16(stream,first+2).unwrap());}
  let entries=parse(child(file,b"cidx").unwrap().unwrap(),start,start+stream.len() as u64,start+first as u64,64).unwrap();assert_eq!(entries.len(),256);
  for entry in entries {let at=entry.offset as usize;assert_eq!(be16(file,at).unwrap(),0xff90);assert_eq!(be16(file,at+4).unwrap(),entry.tile);assert_eq!(be32(file,at+6).unwrap(),entry.length);assert_eq!(file[at+10],entry.ordinal);}
 }
 #[tokio::test]async fn advertised_index_and_unusable_metadata_keep_selected_tiles_correct() {
  use crate::remote_source::{RemoteSource,ReadContext,RangeCache};
  use object_store::{ObjectStore,ObjectStoreExt,path::Path};use std::{sync::{Arc,Mutex},io::Write};
  let runtime=tokio::runtime::Handle::current();
  tokio::task::spawn_blocking(move||{
   let mut operations=vec![];
   for corruption in 0..3 {
    let mut bytes=include_bytes!("../../tests/fixtures/indexed-cidx/reference.jp2").to_vec();
    if corruption==1 {let pointer=child(&bytes,b"iptr").unwrap().unwrap();let at=pointer.as_ptr() as usize-bytes.as_ptr() as usize;bytes[at..at+8].copy_from_slice(&u64::MAX.to_be_bytes());}
    if corruption==2 {
     let faix=child(child(child(&bytes,b"cidx").unwrap().unwrap(),b"tpix").unwrap().unwrap(),b"faix").unwrap().unwrap();
     let first=faix.as_ptr() as usize-bytes.as_ptr() as usize+9;let last=first+63*48;
     let a=bytes[first..first+48].to_vec();let z=bytes[last..last+48].to_vec();bytes[first..first+48].copy_from_slice(&z);bytes[last..last+48].copy_from_slice(&a);
    }
    let mut file=tempfile::NamedTempFile::new().unwrap();file.write_all(&bytes).unwrap();
    // A valid sparse free box exercises the large-object metadata route. This
    // is a functional gate, not a large-imagery performance fixture.
    let padding=9*1024*1024u32;file.write_all(&padding.to_be_bytes()).unwrap();file.write_all(b"free").unwrap();file.as_file().set_len(bytes.len() as u64+u64::from(padding)).unwrap();file.flush().unwrap();
    let path=file.path().to_owned();let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
    let location=Path::from(path.file_name().unwrap().to_str().unwrap());let meta=runtime.block_on(store.head(&location)).unwrap();
    let id=format!("jpip-functional-{}",uuid::Uuid::new_v4());let (source,_)=RemoteSource::open(id.clone(),id,store,location,meta,runtime.clone(),Arc::new(RangeCache {limit:16*1024*1024,state:Mutex::new(Default::default())})).unwrap();
    let mut context=ReadContext::new(None);let basic=source.acquire_header(&mut context).unwrap();
    assert!(basic.jpip.is_empty() && basic.jpip_locator.is_some(),"metadata inspection must defer optional index IO");
    assert_eq!(context.stats.requests,0);
    let payload=source.acquire_render_header(&mut context).unwrap();
    assert!(basic.jpip.is_empty() && basic.jpip_locator.is_some(),"leased metadata snapshot must remain immutable");
    assert!(payload.jpip_locator.is_none());let requests=context.stats.requests;
    assert!(Arc::ptr_eq(&payload,&source.acquire_render_header(&mut context).unwrap()));assert_eq!(context.stats.requests,requests,"materialized index must not be refetched");
    assert_eq!(payload.jpip.is_empty(),corruption==1);
    let parts=source.selected_parts(&std::collections::HashSet::from([63]),&payload,12,12,false,false,&mut context).unwrap();
    assert_eq!(parts.len(),4);assert!(parts.iter().enumerate().all(|(i,p)|p.tile==63 && usize::from(p.ordinal)==i));
    operations.push(context.stats.requests);
    if corruption==0 {
     for layers in [1,0] {
      let mut context=ReadContext::new(None);
      let prepared=source.prepare_multipart_prefix_enabled(&(0..64).collect(),&payload,3,layers,&mut context).unwrap().unwrap();
      assert_eq!(prepared.view_reduction,3);
      let image=jpeg2k::Image::from_bytes_with(&prepared.bytes,jpeg2k::DecodeParameters::new().threads(1).layers(layers)).unwrap();
      let planes=image.components();assert_eq!(planes.len(),1);assert_eq!((planes[0].width(),planes[0].height()),(64,64));
      let gold:&[u8]=if layers==1 {include_bytes!("../../tests/fixtures/indexed-cidx/r3-l1.gray")}else{include_bytes!("../../tests/fixtures/indexed-cidx/r3-all.gray")};
      assert!(planes[0].data().iter().zip(gold).all(|(&actual,&expected)|actual==i32::from(expected)),"indexed prefix must preserve independent reduced quality pixels");
     }
    }
   }
   assert!(operations[0]<operations[1] && operations[0]<operations[2],"direct index must avoid the fallback walk: {operations:?}");
  }).await.unwrap();
 }
}
