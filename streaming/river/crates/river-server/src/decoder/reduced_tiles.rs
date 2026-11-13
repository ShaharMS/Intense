//! Decode independent small native tiles from a reduced technical view.
use super::{Jpeg2000Source,rgba8};
use anyhow::{Result,Context,ensure};
use jpeg2k::{Image,DecodeParameters};
use std::sync::{Mutex,atomic::{AtomicUsize,Ordering}};

pub(super) fn eligible(source:&Jpeg2000Source)->bool {
 let Some(p)=&source.prepared else{return false;};
 let supported=matches!(p.route,"s3-reduced-resolution-view"|"s3-multipart-compact-lowest-prefix")
  || (p.route=="s3-multipart-partial-compact-prefix" && p.original_tiles.len()==1);
 if !supported || p.view_reduction==0 || source.memory.is_none() {return false;}
 let Some(layout)=source.native_tile_layout else{return false;};let scale=1u32<<p.view_reduction;
 let (w,h)=(layout.width.div_ceil(scale),layout.height.div_ceil(scale));
 if u64::from(source.width.div_ceil(scale))*u64::from(source.height.div_ceil(scale))>64*1024*1024 {return false;}
 (128..=16384).contains(&(u64::from(layout.columns)*u64::from(layout.rows))) && (4..=128).contains(&w) && (4..=128).contains(&h)
}
fn u16_at(b:&[u8],at:usize)->Result<u16>{Ok(u16::from_be_bytes(b.get(at..at+2).context("short reduced-tile u16")?.try_into()?))}
fn u32_at(b:&[u8],at:usize)->Result<u32>{Ok(u32::from_be_bytes(b.get(at..at+4).context("short reduced-tile u32")?.try_into()?))}
fn split(bytes:&[u8])->Result<(&[u8],&[u8])> {
 if bytes.starts_with(&[0xff,0x4f]) {return Ok((&[],bytes));}
 let mut at=0;
 while at+8<=bytes.len() {
  let mut length=u64::from(u32_at(bytes,at)?);let mut header=8;
  if length==1 {length=u64::from_be_bytes(bytes.get(at+8..at+16).context("short extended JP2 box")?.try_into()?);header=16;}
  if length==0 {length=(bytes.len()-at) as u64;}
  let length=usize::try_from(length)?;ensure!(length>=header && at.checked_add(length).is_some_and(|n|n<=bytes.len()),"invalid reduced-view JP2 box");
  if &bytes[at+4..at+8]==b"jp2c" {return Ok((&bytes[..at],&bytes[at+header..at+length]));}at+=length;
 }
 anyhow::bail!("reduced view has no codestream")
}
struct Part {at:usize,end:usize,tile:u16}
type Piece=(u32,u32,u32,u32,Vec<u8>);
fn stitch(pieces:Vec<Piece>,area:(u32,u32,u32,u32))->Vec<u8> {
 let (out_w,out_h)=(area.2-area.0,area.3-area.1);let mut rgba=vec![0u8;out_w as usize*out_h as usize*4];
 for (x,y,iw,ih,data) in pieces {
  let (left,right,top,bottom)=(x.max(area.0),(x+iw).min(area.2),y.max(area.1),(y+ih).min(area.3));
  if left>=right || top>=bottom {continue;}
  for row in top..bottom {let dst=((row-area.1) as usize*out_w as usize+(left-area.0) as usize)*4;let src=((row-y) as usize*iw as usize+(left-x) as usize)*4;let n=(right-left) as usize*4;rgba[dst..dst+n].copy_from_slice(&data[src..src+n]);}
 }
 rgba
}
fn requested_jobs(parts:&[Vec<Part>],nx:u32,tw:u32,th:u32,area:(u32,u32,u32,u32))->Result<Vec<usize>> {
 let mut jobs=vec![];
 for y in area.1/th..=(area.3-1)/th {for x in area.0/tw..=(area.2-1)/tw {
  let id=usize::try_from(u64::from(y)*u64::from(nx)+u64::from(x))?;
  ensure!(parts.get(id).is_some_and(|group|!group.is_empty()),"incomplete reduced tile grid for requested area");jobs.push(id);
 }}
 Ok(jobs)
}
fn grouped_jobs(jobs:&[usize],nx:usize,limit:usize)->Vec<Vec<usize>> {
 let mut groups:Vec<Vec<usize>>=vec![];
 for &id in jobs {
  if let Some(last)=groups.last_mut().filter(|g|g.len()<limit.max(1) && g.last().is_some_and(|&previous|previous+1==id && previous/nx==id/nx)) {last.push(id);}
  else {groups.push(vec![id]);}
 }
 groups
}
pub(super) fn decode(source:&Jpeg2000Source,area:(u32,u32,u32,u32),reduction:u8,layers:u32,threads:usize)->Result<Option<(u32,u32,Vec<u8>)>> {
 if threads<2 || reduction!=0 || !eligible(source) {return Ok(None);}
 let (container,cs)=split(source.memory.as_ref().unwrap())?;ensure!(cs.starts_with(&[0xff,0x4f]),"invalid reduced codestream");
 let mut at=2;let mut siz=None;
 while u16_at(cs,at)?!=0xff90 {let marker=u16_at(cs,at)?;let n=usize::from(u16_at(cs,at+2)?);ensure!(n>=2 && at+n+2<=cs.len(),"invalid reduced main header");if marker==0xff51 {siz=Some(at+4);}at+=n+2;}
 let header_end=at;let siz=siz.context("reduced view missing SIZ")?;
 let (w,h,tw,th)=(u32_at(cs,siz+2)?,u32_at(cs,siz+6)?,u32_at(cs,siz+18)?,u32_at(cs,siz+22)?);
 ensure!(tw>0 && th>0 && u32_at(cs,siz+10)?==0 && u32_at(cs,siz+14)?==0,"invalid reduced tile geometry");
 ensure!(area.0<area.2 && area.1<area.3 && area.2<=w && area.3<=h,"reduced requested area exceeds encoded image");
 let (out_w,out_h)=(area.2-area.0,area.3-area.1);
 let pixels=usize::try_from(u64::from(out_w)*u64::from(out_h))?;if pixels>64*1024*1024 {return Ok(None);}
 let nx=w.div_ceil(tw);let count=usize::try_from(u64::from(nx)*u64::from(h.div_ceil(th)))?;ensure!((128..=16384).contains(&count),"reduced tile count exceeds bound");
 let mut parts=(0..count).map(|_|Vec::with_capacity(1)).collect::<Vec<Vec<Part>>>();
 while u16_at(cs,at)?!=0xffd9 {
  ensure!(u16_at(cs,at)?==0xff90 && u16_at(cs,at+2)?==10,"invalid reduced SOT");let n=usize::try_from(u32_at(cs,at+6)?)?;
  ensure!(n>=14 && at.checked_add(n).is_some_and(|end|end<=cs.len()),"invalid reduced tile extent");
  let tile=u16_at(cs,at+4)?;ensure!(usize::from(tile)<count,"reduced tile index outside grid");
  let group=&mut parts[usize::from(tile)];ensure!(group.len()<256 && usize::from(cs[at+10])==group.len(),"reduced tile-part ordinals are missing or duplicated");
  group.push(Part{at,end:at+n,tile});at+=n;
 }
 ensure!(at+2==cs.len(),"invalid trailing bytes in reduced tile grid");
 for group in &parts {ensure!(group.iter().all(|part|cs[part.at+11]==0 || usize::from(cs[part.at+11])==group.len()),"reduced tile-part total disagrees with its chain");}
 let requested=requested_jobs(&parts,nx,tw,th,area)?;
 let batch=std::env::var("RIVER_MAX_NATIVE_TILES_PER_JOB").ok().and_then(|v|v.parse::<usize>().ok()).filter(|&v|v>0).unwrap_or(4).min(4);
 let jobs=grouped_jobs(&requested,nx as usize,batch);
 if std::env::var_os("RIVER_PROFILE_SOURCE").is_some() {tracing::info!(native_tiles=requested.len(),groups=jobs.len(),workers=threads.min(jobs.len()),main_header_bytes=header_end,container_bytes=container.len(),"OpenJPEG reduced tile workload");}
 let next=AtomicUsize::new(0);let results=Mutex::new(Vec::with_capacity(jobs.len()));let error=Mutex::new(None);
 let worker=|| {
   loop {
    if error.lock().unwrap().is_some() {break;}let i=next.fetch_add(1,Ordering::Relaxed);let Some(indices)=jobs.get(i) else{break;};let tile=parts[indices[0]][0].tile;
    let result=(||->Result<_>{
     let (x,y)=(u32::from(tile)%nx*tw,u32::from(tile)/nx*th);let last=*indices.last().unwrap() as u32;let (x1,y1)=(((last%nx+1)*tw).min(w),(y+th).min(h));
     let mut stream=cs[..header_end].to_vec();for (j,n) in [x1,y1,x,y,tw,th,x,y].into_iter().enumerate(){stream[siz+2+j*4..siz+6+j*4].copy_from_slice(&n.to_be_bytes());}
     for (local,&index) in indices.iter().enumerate() {for part in &parts[index] {let begin=stream.len();stream.extend_from_slice(&cs[part.at..part.end]);stream[begin+4..begin+6].copy_from_slice(&u16::try_from(local)?.to_be_bytes());}}stream.extend([0xff,0xd9]);
     let bytes=if container.is_empty(){stream}else{let mut prefix=container.to_vec();crate::remote_source::resize_jp2(&mut prefix,x1-x,y1-y)?;prefix.extend(u32::try_from(stream.len()+8)?.to_be_bytes());prefix.extend(b"jp2c");prefix.extend(stream);prefix};
     let image=Image::from_bytes_with(&bytes,DecodeParameters::new().threads(1).layers(layers)).with_context(||format!("OpenJPEG reduced native tile group {:?} failed",indices))?;
     let (iw,ih,rgba)=if let Some(rgb)=super::pixels::rgb8(&image){rgb}else{rgba8(image.get_pixels(None).context("reduced native tile color conversion failed")?)?};
     ensure!((iw,ih)==(x1-x,y1-y),"reduced native tile dimensions changed");Ok((x,y,iw,ih,rgba))
    })();
    match result {Ok(piece)=>results.lock().unwrap().push(piece),Err(e)=>{*error.lock().unwrap()=Some(e);break;}}
   }
 };
 std::thread::scope(|scope| {
  for _ in 1..threads.min(jobs.len()) {
   if let Err(e)=std::thread::Builder::new().name("river-reduced-tile".into()).spawn_scoped(scope,&worker) {*error.lock().unwrap()=Some(anyhow::Error::new(e).context("cannot start reduced-tile decoder"));break;}
  }
  worker();
 });
 if let Some(error)=error.into_inner().unwrap(){return Err(error);}
 let rgba=stitch(results.into_inner().unwrap(),area);
 Ok(Some((out_w,out_h,rgba)))
}

pub(super) fn render(source:&Jpeg2000Source,tiles:&[(u32,u32)],source_tile:u32,reduce:u8,layers:u32,format:super::RasterFormat,threads:usize)->Result<Option<Vec<super::DecodedTile>>> {
 if tiles.is_empty(){return Ok(Some(vec![]));}
 let regions=tiles.iter().map(|&(x,y)|{let (x0,x1)=super::clamped_tile_bounds(x,source_tile,source.width);let(y0,y1)=super::clamped_tile_bounds(y,source_tile,source.height);(x,y,x0,y0,x1,y1)}).collect::<Vec<_>>();
 let x0=regions.iter().map(|r|r.2).min().unwrap();let y0=regions.iter().map(|r|r.3).min().unwrap();let x1=regions.iter().map(|r|r.4).max().unwrap();let y1=regions.iter().map(|r|r.5).max().unwrap();
 ensure!(x0<x1 && y0<y1,"requested JPEG2000 tile union is empty");
 let began=std::time::Instant::now();let Some((w,h,rgba))=decode(source,source.reference_area((x0,y0,x1,y1))?,source.technical_reduction(reduce)?,layers,threads)? else{return Ok(None);};
 let decode_ms=super::elapsed_ms(began);let scale=1u32<<reduce;
 let decoded=regions.into_iter().map(|(x,y,a,b,c,d)| {
  let dx=(a-x0)/scale;let dy=(b-y0)/scale;let width=(w-dx).min((c-a).div_ceil(scale));let height=(h-dy).min((d-b).div_ceil(scale));
  ensure!(width>0 && height>0,"reduced native tile lies outside decoded union");
  let began=std::time::Instant::now();let bytes=super::encode_crop_rgba(&rgba,w,dx,dy,width,height,format)?;
  Ok(super::DecodedTile {tile_x:x,tile_y:y,rgba:super::take_pixel_capture(),raster:super::DecodedRaster {width,height,bytes:bytes.into_boxed_slice().into(),content_type:format.content_type(),decoder_backend: "openjpeg", decoder_route:"openjpeg-parallel-reduced",decode_ms,encode_ms:super::elapsed_ms(began),source_cache_status:"bypass",source_cache_ms:0.0,compressed_resolution_view:false,source_read_bytes:0,source_range_requests:0,source_cache_hit_bytes:0}})
 }).collect::<Result<Vec<_>>>()?;
 Ok(Some(decoded))
}

#[cfg(test)] mod tests {
 use super::*;
 #[test] fn codec_groups_do_not_cross_rows_gaps_or_configured_limits() {
  assert_eq!(grouped_jobs(&[0,1,2,3,4,5,7,8,9],4,4),vec![vec![0,1,2,3],vec![4,5],vec![7],vec![8,9]]);
  assert_eq!(grouped_jobs(&[0,1,2,3],4,1),vec![vec![0],vec![1],vec![2],vec![3]]);
  assert_eq!(grouped_jobs(&[0,1,2,3],4,2),vec![vec![0,1],vec![2,3]]);
 }
 fn invalid_chain(ordinal:u8,total:u8,missing_last:bool)->Jpeg2000Source {
  let mut bytes=vec![0xff,0x4f];let mut siz=vec![0,0];
  for value in [256u32,128,0,0,16,16,0,0] {siz.extend(value.to_be_bytes());}
  siz.extend([0,1,7,1,1]);bytes.extend([0xff,0x51]);bytes.extend(((siz.len()+2) as u16).to_be_bytes());bytes.extend(siz);
  for tile in 0..if missing_last {127u16}else{128u16} {
   bytes.extend([0xff,0x90,0,10]);bytes.extend(tile.to_be_bytes());bytes.extend(14u32.to_be_bytes());bytes.extend([if tile==0 {ordinal}else{0},total,0xff,0x93]);
  }
  bytes.extend([0xff,0xd9]);let mut source=Jpeg2000Source::from_path("chain-test.j2k",512,256);
  source.memory=Some(bytes.into());source.native_tile_layout=Some(super::super::NativeTileLayoutHint {origin_x:0,origin_y:0,width:32,height:32,columns:16,rows:8});
  source.prepared=Some(std::sync::Arc::new(super::super::PreparedIo {file_ranges:false,view_reduction:1,pcrl_lowest_revision:false,technical_origin:None,predecoded:None,overlapped_decode_ms:0.0,predecoded_origin:(0,0),original_path:source.path.clone(),original_tiles:vec![(0,0)],tile_offset_x:0,tile_offset_y:0,route:"s3-multipart-compact-lowest-prefix",stats:Default::default(),elapsed_ms:0.0,_file:None}));source
 }
 #[test] fn rejects_nonconsecutive_parts_before_decoder_creation() {
  let error=decode(&invalid_chain(1,1,false),(0,0,256,128),0,0,2).unwrap_err();assert!(error.to_string().contains("ordinals"));
 }
 #[test] fn rejects_inconsistent_part_totals_before_decoder_creation() {
  let error=decode(&invalid_chain(0,2,false),(0,0,256,128),0,0,2).unwrap_err();assert!(error.to_string().contains("total disagrees"));
 }
 #[test] fn rejects_missing_native_tiles_before_decoder_creation() {
  let error=decode(&invalid_chain(0,1,true),(0,0,256,128),0,0,2).unwrap_err();assert!(error.to_string().contains("incomplete reduced tile grid"));
 }
 #[test] fn sparse_grids_require_only_tiles_intersecting_the_requested_rectangle() {
  let mut groups=(0..8).map(|_|vec![]).collect::<Vec<Vec<Part>>>();groups[5].push(Part {at:0,end:14,tile:5});
  assert_eq!(requested_jobs(&groups,4,16,16,(17,17,31,31)).unwrap(),vec![5]);
  assert!(requested_jobs(&groups,4,16,16,(0,0,16,16)).unwrap_err().to_string().contains("requested area"));
 }
 #[test] fn sparse_stitch_clips_all_edges_without_moving_pixels() {
  let mut pieces=vec![];
  for ty in 0..2 {for tx in 0..2 {let mut data=vec![];
   for y in ty*4..ty*4+4 {for x in tx*4..tx*4+4 {data.extend([x as u8,y as u8,17,255]);}}
   pieces.push((tx*4,ty*4,4,4,data));
  }}
  let actual=stitch(pieces,(1,2,7,7));let mut expected=vec![];
  for y in 2..7 {for x in 1..7 {expected.extend([x,y,17,255]);}}
  assert_eq!(actual,expected);
 }
}
