//! Diagnostic only: decoder callbacks use the same budgeted range reader.
use super::*;
use std::io::{Read,Seek,SeekFrom};
struct Reader<'a> {
 source:&'a RemoteSource,context:&'a mut ReadContext,cursor:u64,
 start:u64,window:Vec<u8>,quantum:u64,error:Option<String>,
}
#[tokio::test]
async fn tiny_group_clipped_u16_matches_independent_kakadu_planes() {
 use std::io::Write;
 let runtime=tokio::runtime::Handle::current();
 tokio::task::spawn_blocking(move||{
  let fixtures=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-groups16-kdu444");
  for (name,channels) in [("gray16",1usize),("rgb16",3)] {
   let bytes=std::fs::read(fixtures.join(format!("{name}.jp2"))).unwrap();let mut file=tempfile::NamedTempFile::new().unwrap();file.write_all(&bytes).unwrap();
   let padding=9*1024*1024u32;file.write_all(&padding.to_be_bytes()).unwrap();file.write_all(b"free").unwrap();file.as_file().set_len(bytes.len() as u64+u64::from(padding)).unwrap();file.flush().unwrap();
   let path=file.path().to_owned();let store:Arc<dyn ObjectStore>=Arc::new(object_store::local::LocalFileSystem::new_with_prefix(path.parent().unwrap()).unwrap());
   let location=Path::from(path.file_name().unwrap().to_str().unwrap());let meta=runtime.block_on(store.head(&location)).unwrap();let id=format!("tiny-format-{}",uuid::Uuid::new_v4());
   let (source,_)=RemoteSource::open(id.clone(),id,store,location,meta,runtime.clone(),Arc::new(RangeCache {limit:16*1024*1024,state:Mutex::new(CacheState::default())})).unwrap();
   assert_eq!(source.header.bit_depth,16);assert_eq!(source.header.cod[4],if channels==3 {1}else{0});
   let mut context=ReadContext::new(None);let payload=source.acquire_render_header(&mut context).unwrap();let selected=(0..128).collect();
   let parts=source.selected_parts(&selected,&payload,16384,14,false,true,&mut context).unwrap();
   for layers in [0,1] {
    let prepared=source.prepare_discovered_multipart_prefix(&selected,&payload,&parts,2,layers,&mut context).unwrap().unwrap();
    let mut view=crate::decoder::Jpeg2000Source::from_catalog("tiny-format.j2k",509,251,Some(source.header.layout));view.memory=Some(prepared.bytes.into());
    view.prepared=Some(Arc::new(crate::decoder::PreparedIo {file_ranges:false,view_reduction:2,pcrl_lowest_revision:false,technical_origin:None,predecoded:None,overlapped_decode_ms:0.0,predecoded_origin:(0,0),original_path:view.path.clone(),original_tiles:vec![(0,0)],tile_offset_x:0,tile_offset_y:0,route:prepared.route,stats:Default::default(),elapsed_ms:0.0,_file:None}));
    let planes=(0..channels).map(|c|std::fs::read(fixtures.join(format!("{name}-r2-l{layers}-{c}.u16be"))).unwrap().chunks_exact(2).map(|b|u16::from_be_bytes([b[0],b[1]])).collect::<Vec<_>>()).collect::<Vec<_>>();
    let mut rgba=vec![];for i in 0..128*63 {for c in 0..3 {let value=planes[if channels==1 {0}else{c}][i];rgba.push(((u32::from(value)*255+32767)/65535) as u8);}rgba.push(255);}
    let gold=image::RgbaImage::from_raw(128,63,rgba).unwrap();
    for (tile,size,rect) in [((0,0),512,(0,0,128,63)),((3,1),128,(96,32,32,31)),((1,1),100,(25,25,25,25))] {for threads in [1,3] {
     let decoded=view.decode_tiles_with_threads(&[tile],size,2,layers,river_core::RasterFormat::Png,threads).unwrap();
     let actual=image::load_from_memory(&decoded[0].raster.bytes).unwrap().to_rgba8();
     assert_eq!(actual,image::imageops::crop_imm(&gold,rect.0,rect.1,rect.2,rect.3).to_image(),"{name} layers{layers} tile{tile:?} threads{threads}");
     if threads==3 {assert_eq!(decoded[0].raster.decoder_route,"openjpeg-parallel-reduced");}
    }}
   }
  }
 }).await.unwrap();
}
impl Read for Reader<'_> {
 fn read(&mut self,out:&mut [u8])->std::io::Result<usize> {
  let result=(||->Result<usize>{
   self.context.check()?;
   if out.is_empty() || self.cursor>=self.source.meta.size{return Ok(0);}
   if self.window.is_empty() || self.cursor<self.start || self.cursor>=self.start+self.window.len() as u64 {
    self.start=self.cursor/self.quantum*self.quantum;
    self.window=self.source.read(self.start..(self.start+self.quantum).min(self.source.meta.size),self.context)?;
   }
   let offset=(self.cursor-self.start) as usize;let n=out.len().min(self.window.len()-offset);
   out[..n].copy_from_slice(&self.window[offset..offset+n]);self.cursor+=n as u64;Ok(n)
  })();
  result.map_err(|e|{let text=format!("{e:#}");self.error=Some(text.clone());std::io::Error::other(text)})
 }
}
impl Seek for Reader<'_> {
 fn seek(&mut self,at:SeekFrom)->std::io::Result<u64> {
  let next=match at {SeekFrom::Start(n)=>i128::from(n),SeekFrom::Current(n)=>i128::from(self.cursor)+i128::from(n),SeekFrom::End(n)=>i128::from(self.source.meta.size)+i128::from(n)};
  if next<0 || next>i128::from(self.source.meta.size){return Err(std::io::Error::other("decoder seek outside source"));}
  self.cursor=next as u64;Ok(self.cursor)
 }
}
#[tokio::test]
#[ignore="Requires the immutable generated14MBHTTPdiagnostic origin on18969"]
async fn decoder_driven_http_range_probe() {
 let runtime=tokio::runtime::Handle::current();
 tokio::task::spawn_blocking(move||{
  let gold=image::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/unindexed-http-m400/gold.png")).unwrap().to_luma8();
  for layout in ["resolution-major","tile-major"] {for quantum in [65536u64,1048576] {
   let sid=format!("seekable412-{}",uuid::Uuid::new_v4());
   let store:Arc<dyn ObjectStore>=Arc::new(object_store::aws::AmazonS3Builder::new().with_bucket_name("fixture").with_region("us-east-1")
    .with_endpoint(format!("http://127.0.0.1:18969/{sid}")).with_allow_http(true).with_access_key_id("fixture").with_secret_access_key("fixture").build().unwrap());
   let location=Path::from(format!("{layout}.jp2"));let meta=runtime.block_on(store.head(&location)).unwrap();let len=meta.size;
   let (source,_)=RemoteSource::open(sid.clone(),sid.clone(),store,location,meta,runtime.clone(),Arc::new(RangeCache {limit:16*1024*1024,state:Mutex::new(CacheState::default())})).unwrap();
   let mut context=ReadContext::new(None);let mut reader=Reader {source:&source,context:&mut context,cursor:0,start:0,window:vec![],quantum,error:None};
   let image=jpeg2k::Image::from_reader_with(&mut reader,len,jpeg2k::DecodeParameters::new().reduce(3).threads(4));
   let error=reader.error.clone();drop(reader);let image=image.unwrap_or_else(|e|panic!("decoder={e}; source={error:?}"));
   let components=image.components();assert_eq!((components[0].width(),components[0].height()),gold.dimensions());
   assert!(components[0].data().iter().zip(gold.as_raw()).all(|(&a,&b)|a==i32::from(b)));
   let stats=std::process::Command::new("python").args(["-c","import urllib.request,sys;print(urllib.request.urlopen(sys.argv[1]).read().decode())",&format!("http://127.0.0.1:18969/__stats/{sid}")]).output().unwrap();assert!(stats.status.success());
   let wire:serde_json::Value=serde_json::from_slice(&stats.stdout).unwrap();
   eprintln!("{}",serde_json::json!({"layout":layout,"quantum":quantum,"pixelsExact":true,"wire":wire["wire"],"contextRequests":context.stats.requests,"contextBytes":context.stats.bytes}));
  }}
 }).await.unwrap();
}

#[tokio::test]
#[ignore="Requires the immutable generated14MBHTTPdiagnostic origin on18969"]
async fn discovered_prefix_http_range_probe() {
 let runtime=tokio::runtime::Handle::current();
 tokio::task::spawn_blocking(move||{
  let gold=image::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/unindexed-http-m400/gold.png")).unwrap().to_luma8();
  let sid=format!("discovered413-{}",uuid::Uuid::new_v4());
  let store:Arc<dyn ObjectStore>=Arc::new(object_store::aws::AmazonS3Builder::new().with_bucket_name("fixture").with_region("us-east-1")
   .with_endpoint(format!("http://127.0.0.1:18969/{sid}")).with_allow_http(true).with_access_key_id("fixture").with_secret_access_key("fixture").build().unwrap());
  let location=Path::from("tile-major.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
  let (source,_)=RemoteSource::open(sid.clone(),sid.clone(),store,location,meta,runtime.clone(),Arc::new(RangeCache {limit:16*1024*1024,state:Mutex::new(CacheState::default())})).unwrap();
  let mut context=ReadContext::new(None);let payload=source.acquire_render_header(&mut context).unwrap();let selected=(0..256).collect();
  let scan=std::env::var("RIVER_PROBE_SCAN_BYTES").ok().and_then(|v|v.parse::<u64>().ok()).unwrap_or(16384).clamp(4096,1048576);
  let began=Instant::now();
  let parts=source.selected_parts(&selected,&payload,scan,14,false,true,&mut context).unwrap();
  let discovery_ms=began.elapsed().as_secs_f64()*1000.0;
  assert_eq!(parts.len(),1024);
  let prepared=source.prepare_discovered_multipart_prefix(&selected,&payload,&parts,3,0,&mut context).unwrap().unwrap();
  assert_eq!(prepared.view_reduction,3);
  let image=jpeg2k::Image::from_bytes_with(&prepared.bytes,jpeg2k::DecodeParameters::new().threads(4)).unwrap();let components=image.components();
  assert_eq!((components[0].width(),components[0].height()),gold.dimensions());assert!(components[0].data().iter().zip(gold.as_raw()).all(|(&a,&b)|a==i32::from(b)));
  let stats=std::process::Command::new("python").args(["-c","import urllib.request,sys;print(urllib.request.urlopen(sys.argv[1]).read().decode())",&format!("http://127.0.0.1:18969/__stats/{sid}")]).output().unwrap();assert!(stats.status.success());
  let wire:serde_json::Value=serde_json::from_slice(&stats.stdout).unwrap();
  eprintln!("{}",serde_json::json!({"layout":"tile-major","scanBytes":scan,"discoveryMs":discovery_ms,"pixelsExact":true,"wire":wire["wire"],"contextRequests":context.stats.requests,"contextBytes":context.stats.bytes,"preparedBytes":prepared.bytes.len()}));
 }).await.unwrap();
}

#[tokio::test]
#[ignore="Requires hash-pinned virtual14GBunindexed HTTP origin on18968"]
async fn next_level_large_unindexed_prefix_probe() {
 let runtime=tokio::runtime::Handle::current();
 tokio::task::spawn_blocking(move||{
  let gold=image::open(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reports/kakadu-io-m303/large-tlm-style4-diagnostic/gold-preview-r7.png")).unwrap().to_rgb8();
  let sid=format!("next421-{}",uuid::Uuid::new_v4());
  let store:Arc<dyn ObjectStore>=Arc::new(object_store::aws::AmazonS3Builder::new().with_bucket_name("fixture").with_region("us-east-1")
   .with_endpoint(format!("http://127.0.0.1:18968/{sid}")).with_allow_http(true).with_access_key_id("fixture").with_secret_access_key("fixture").build().unwrap());
  let location=Path::from("unindexed-large.jp2");let meta=runtime.block_on(store.head(&location)).unwrap();
  let (source,_)=RemoteSource::open(sid.clone(),sid.clone(),store,location,meta,runtime.clone(),Arc::new(RangeCache {limit:16*1024*1024,state:Mutex::new(CacheState::default())})).unwrap();
  let mut context=ReadContext::new(None);let payload=source.acquire_render_header(&mut context).unwrap();assert!(payload.tlm.is_empty() && payload.jpip.is_empty());
  let limits=(context.limit,context.requests_limit,context.explicit_request_limit);
  let prepared=source.prepare_unindexed_coarse_prefix_enabled(&(0..4096).collect(),&payload,7,0,&mut context).unwrap().unwrap();
  assert_eq!((context.limit,context.requests_limit,context.explicit_request_limit),limits);
  let image=jpeg2k::Image::from_bytes_with(&prepared.bytes,jpeg2k::DecodeParameters::new().threads(4)).unwrap();let components=image.components();
  assert_eq!((components[0].width(),components[0].height()),gold.dimensions());assert_eq!(components.len(),3);
  for c in 0..3 {assert!(components[c].data().iter().zip(gold.pixels()).all(|(&a,b)|a==i32::from(b.0[c])));}
  if std::env::var("RIVER_PROBE_TINY_DECODE").as_deref()==Ok("1") {
   use crate::decoder::{Jpeg2000Source,PreparedIo};
   let mut view=Jpeg2000Source::from_catalog("tiny-probe.j2k",source.header.width,source.header.height,Some(source.header.layout));
   view.memory=Some(prepared.bytes.clone().into());
   view.prepared=Some(Arc::new(PreparedIo {file_ranges:false,view_reduction:7,pcrl_lowest_revision:false,technical_origin:None,predecoded:None,overlapped_decode_ms:0.0,predecoded_origin:(0,0),original_path:view.path.clone(),original_tiles:vec![(0,0)],tile_offset_x:0,tile_offset_y:0,route:prepared.route,stats:Default::default(),elapsed_ms:0.0,_file:None}));
   for pair in 0..6 {for threads in if pair%2==0 {[1,3]}else{[3,1]} {
    let start=Instant::now();let tiles=view.decode_tiles_with_threads(&[(0,0)],65536,7,0,river_core::RasterFormat::Png,threads).unwrap();let ms=start.elapsed().as_secs_f64()*1000.0;
    let actual=image::load_from_memory(&tiles[0].raster.bytes).unwrap().to_rgb8();assert_eq!(actual,gold);
    eprintln!("{}",serde_json::json!({"pair":pair,"threads":threads,"decodeAndEncodeMs":ms,"pixelsExact":true,"route":tiles[0].raster.decoder_route}));
   }}
  }
  let stats=std::process::Command::new("python").args(["-c","import urllib.request,sys;print(urllib.request.urlopen(sys.argv[1]).read().decode())",&format!("http://127.0.0.1:18968/__stats/{sid}")]).output().unwrap();assert!(stats.status.success());
  let wire:serde_json::Value=serde_json::from_slice(&stats.stdout).unwrap();
  eprintln!("{}",serde_json::json!({"reduction":7,"pixelsExact":true,"wire":wire["wire"],"contextRequests":context.stats.requests,"contextBytes":context.stats.bytes,"preparedBytes":prepared.bytes.len()}));
 }).await.unwrap();
}
