//! Exact RGBA8 conversion directly from decoded unsigned RGB planes.
use jpeg2k::{ColorSpace,Image};
pub(super) fn rgb8(image:&Image)->Option<(u32,u32,Vec<u8>)> {
 if !matches!(image.color_space(),ColorSpace::Unknown|ColorSpace::Unspecified|ColorSpace::SRGB|ColorSpace::Gray) {return None;}
 let [r,g,b]=image.components() else{return None;};
 let (width,height)=(r.width(),r.height());
 let precision=r.precision();
 if !matches!(precision,8|12|16) || [r,g,b].iter().any(|c|c.precision()!=precision || c.is_signed() || c.is_alpha() || (c.width(),c.height())!=(width,height)) {return None;}
 let pixels=usize::try_from(width).ok()?.checked_mul(usize::try_from(height).ok()?)?;
 let rgba=match precision {16=>interleave16(r.data(),g.data(),b.data(),pixels)?,12=>interleave12(r.data(),g.data(),b.data(),pixels)?,_=>interleave(r.data(),g.data(),b.data(),pixels)?};
 Some((width,height,rgba))
}
fn interleave12(r:&[i32],g:&[i32],b:&[i32],pixels:usize)->Option<Vec<u8>> {
 if r.len()!=pixels || g.len()!=pixels || b.len()!=pixels {return None;}
 pixels.checked_mul(4)?;
 // Preserve the wrapper's floor-to-u16 normalization followed by River's
 // rounded u16-to-u8 scaling, including its unsigned cast on malformed samples.
 // A direct rounded12-to8 formula can differ at rounding boundaries.
 let scale=|value:i32|super::u16_to_u8((u64::from(value as u32)*65535/4095) as u16);
 Some(r.iter().zip(g).zip(b).flat_map(|((&r,&g),&b)|[scale(r),scale(g),scale(b),255]).collect())
}
fn interleave16(r:&[i32],g:&[i32],b:&[i32],pixels:usize)->Option<Vec<u8>> {
 if r.len()!=pixels || g.len()!=pixels || b.len()!=pixels {return None;}
 pixels.checked_mul(4)?;
 // For unsigned16, legacy data_u16 normalization is identity modulo u16.
 // Preserve that cast and River's rounded scaling, without an RGB16 buffer.
 Some(r.iter().zip(g).zip(b).flat_map(|((&r,&g),&b)|[
  super::u16_to_u8(r as u16),super::u16_to_u8(g as u16),super::u16_to_u8(b as u16),255]).collect())
}
fn interleave(r:&[i32],g:&[i32],b:&[i32],pixels:usize)->Option<Vec<u8>> {
 if r.len()!=pixels || g.len()!=pixels || b.len()!=pixels {return None;}
 pixels.checked_mul(4)?;
 // Typed exact-length iterators allocate once without zero-filling an RGBA
 // buffer that will be completely overwritten. Preserve legacy wrapping.
 Some(r.iter().zip(g).zip(b).flat_map(|((&r,&g),&b)|[r as u8,g as u8,b as u8,255]).collect())
}
#[cfg(test)] mod tests {
 use super::{interleave,interleave12,interleave16};
 #[test] fn rgb12_matches_legacy_normalization_for_every_value_and_wrapping_edges() {
  let r=(0..=4095).chain([i32::MIN,-1,4096,65535,65536,i32::MAX]).collect::<Vec<i32>>();
  let g=r.iter().rev().copied().collect::<Vec<_>>();let b=r.iter().map(|v|v.wrapping_mul(7919)).collect::<Vec<_>>();
  let normalized=|v:i32|((u64::from(v as u32)*65535)/4095) as u16;
  let data=r.iter().zip(&g).zip(&b).flat_map(|((&r,&g),&b)|[normalized(r),normalized(g),normalized(b)]).collect();
  let (_,_,expected)=super::super::rgba8(jpeg2k::ImageData {width:r.len() as u32,height:1,format:jpeg2k::ImageFormat::Rgb16,data:jpeg2k::ImagePixelData::Rgb16(data)}).unwrap();
  assert_eq!(interleave12(&r,&g,&b,r.len()).unwrap(),expected);
  assert!(interleave12(&[1],&[],&[1],1).is_none());
  assert!(interleave12(&[],&[],&[],usize::MAX).is_none());
 }
 #[test] fn decoded_rgb12_mct_variants_match_the_wrapper_pixel_path() {
  for bytes in [include_bytes!("../../tests/fixtures/rgb12/mct0.jp2").as_slice(),include_bytes!("../../tests/fixtures/rgb12/mct1.jp2").as_slice()] {
   let image=jpeg2k::Image::from_bytes(bytes).unwrap();
   assert!(image.components().iter().all(|c|c.precision()==12));
   let expected=super::super::rgba8(image.get_pixels(None).unwrap()).unwrap();
   assert_eq!(super::rgb8(&image).unwrap(),expected);
  }
 }
 #[test]
 #[ignore="Conversion-only release benchmark; decoded12bit diagnostic,not end-to-end latency"]
 fn decoded_rgb12_conversion_benchmark() {
  let image=jpeg2k::Image::from_bytes(include_bytes!("../../tests/fixtures/rgb12/mct1.jp2")).unwrap();
  let legacy=||super::super::rgba8(image.get_pixels(None).unwrap()).unwrap();
  assert_eq!(super::rgb8(&image).unwrap(),legacy());
  for pair in 0..6 {for direct in if pair%2==0 {[false,true]}else{[true,false]} {
   let started=std::time::Instant::now();
   for _ in 0..1024 {std::hint::black_box(if direct {super::rgb8(&image).unwrap()}else{legacy()});}
   eprintln!("RGB12_BENCH {}",serde_json::json!({"pair":pair,"direct":direct,"ns":started.elapsed().as_nanos(),"repeats":1024,"pixelsPerRepeat":u64::from(image.components()[0].width())*u64::from(image.components()[0].height()),"pixelsExact":true}));
  }}
 }
 #[test] fn rgb16_matches_legacy_for_every_unsigned_value_and_wrapping_edges() {
  let r=(0..=65535).chain([i32::MIN,-1,65536,i32::MAX]).collect::<Vec<i32>>();
  let g=r.iter().rev().copied().collect::<Vec<_>>();let b=r.iter().map(|v|v.wrapping_mul(7919)).collect::<Vec<_>>();
  let data=r.iter().zip(&g).zip(&b).flat_map(|((&r,&g),&b)|[r as u16,g as u16,b as u16]).collect();
  let (_,_,expected)=super::super::rgba8(jpeg2k::ImageData {width:r.len() as u32,height:1,format:jpeg2k::ImageFormat::Rgb16,data:jpeg2k::ImagePixelData::Rgb16(data)}).unwrap();
  assert_eq!(interleave16(&r,&g,&b,r.len()).unwrap(),expected);
 }
 #[test] fn rgb_planes_preserve_channel_order_alpha_and_legacy_scaling() {
  let r=[0,127,255,-1,256,i32::MIN,i32::MAX];let g=[255,128,0,256,-1,i32::MAX,i32::MIN];let b=[17;7];
  let actual=interleave(&r,&g,&b,7).unwrap();
  let scale=|v:i32|((u64::from(v as u32)*255)/255) as u8;
  let expected:Vec<u8>=(0..7).flat_map(|i|[scale(r[i]),scale(g[i]),scale(b[i]),255]).collect();
  assert_eq!(actual,expected);
 }
 #[test] fn mismatched_planes_never_produce_a_partial_raster() {
  assert!(interleave(&[1],&[],&[1],1).is_none());
  assert!(interleave(&[],&[],&[],usize::MAX).is_none());
  assert_eq!(interleave(&[],&[],&[],0),Some(vec![]));
  assert!(interleave16(&[1],&[],&[1],1).is_none());
  assert!(interleave16(&[],&[],&[],usize::MAX).is_none());
 }
 #[test]
 #[ignore="Requires the hash-pinned RGB16 JP2 diagnostic; release conversion benchmark only"]
 fn decoded_rgb16_conversion_benchmark() {
  use sha2::{Digest,Sha256};
  let path=std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../reports/fine-precincts-m329/source16-kakadu.jp2");
  let bytes=std::fs::read(path).unwrap();assert_eq!(hex::encode(Sha256::digest(&bytes)),"5790e8a1d4b26c936006ac1f94bd2110b6f598210cb60070ac2df9b97d9ac242");
  let image=jpeg2k::Image::from_bytes(&bytes).unwrap();
  let legacy=||super::super::rgba8(image.get_pixels(None).unwrap()).unwrap();
  let expected=legacy();let actual=super::rgb8(&image).expect("RGB16 should use direct conversion");assert_eq!(actual,expected);
  let pixels=u64::from(actual.0)*u64::from(actual.1);drop(actual);drop(expected);
  for pair in 0..6 {for direct in if pair%2==0 {[false,true]}else{[true,false]} {
   let start=std::time::Instant::now();
   for _ in 0..4 {std::hint::black_box(if direct {super::rgb8(&image).unwrap()}else{legacy()});}
   eprintln!("RGB16_BENCH {}",serde_json::json!({"pair":pair,"direct":direct,"ns":start.elapsed().as_nanos(),"pixels":pixels,"intermediateRgb16BytesAvoided":if direct {pixels*6}else{0},"pixelsExact":true}));
  }}
 }
}
