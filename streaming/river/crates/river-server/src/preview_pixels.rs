//! Stitch normalized decoded pixels, never encoded/lossy patches.
use std::{collections::HashSet,sync::{OnceLock,atomic::{AtomicUsize,Ordering}}};
use crate::{decoder::{DecodedTile,DecodedRaster},engine::TileDecodeRequest};
static USED:AtomicUsize=AtomicUsize::new(0);
fn limit()->usize {static LIMIT:OnceLock<usize>=OnceLock::new();*LIMIT.get_or_init(||std::env::var("RIVER_PREVIEW_PIXEL_REUSE_BYTES").ok().and_then(|v|v.parse().ok()).unwrap_or(32*1024*1024))}
pub struct Pixels {width:u32,height:u32,edge:u32,scale:u32,data:Vec<u8>,seen:HashSet<(u32,u32)>,valid:bool,charge:usize}
impl Drop for Pixels {fn drop(&mut self) {USED.fetch_sub(self.charge,Ordering::AcqRel);}}
impl Pixels {
    pub fn new(request:&TileDecodeRequest,edge:u32)->Option<Self> {
        let scale=1u32.checked_shl(u32::from(request.reduction))?;
        if request.source.reference_origin!=(0,0) || edge<scale || edge%scale!=0 {return None;}
        let width=request.source.width.div_ceil(scale);let height=request.source.height.div_ceil(scale);
        let count=u64::from(width)*u64::from(height);
        if count==0 || count>4*1024*1024 {return None;}
        let charge=(count as usize)*4+128*1024;
        USED.fetch_update(Ordering::AcqRel,Ordering::Acquire,|used|used.checked_add(charge).filter(|total|*total<=limit())).ok()?;
        Some(Self {width,height,edge,scale,data:vec![0;count as usize*4],seen:HashSet::new(),valid:true,charge})
    }
    pub fn add(&mut self,tile:&DecodedTile) {
        if !self.valid {return;}
        let Some(data)=tile.rgba.as_deref() else {self.valid=false;return;};
        let x=u64::from(tile.tile_x)*u64::from(self.edge)/u64::from(self.scale);
        let y=u64::from(tile.tile_y)*u64::from(self.edge)/u64::from(self.scale);
        if x>=u64::from(self.width) || y>=u64::from(self.height) || !self.seen.insert((tile.tile_x,tile.tile_y)) {self.valid=false;return;}
        let x=x as u32;let y=y as u32;
        let w=(self.edge/self.scale).min(self.width-x);let h=(self.edge/self.scale).min(self.height-y);
        if (w,h)!=(tile.raster.width,tile.raster.height) || data.len()!=w as usize*h as usize*4 {self.valid=false;return;}
        for row in 0..h as usize {
            let dest=((y as usize+row)*self.width as usize+x as usize)*4;
            self.data[dest..dest+w as usize*4].copy_from_slice(&data[row*w as usize*4..(row+1)*w as usize*4]);
        }
    }
    pub fn complete(&self)->bool {
        self.valid && self.seen.len()==self.width.div_ceil(self.edge/self.scale) as usize*self.height.div_ceil(self.edge/self.scale) as usize
    }
    pub fn encode(&self,request:&TileDecodeRequest)->anyhow::Result<Vec<DecodedTile>> {
        anyhow::ensure!(self.complete(),"preview pixels incomplete");
        let _encoding=crate::encoding_settings::Scope::new(request.webp_method);
        let _capture=crate::decoder::PixelCapture::new(false);
        request.tiles.iter().map(|&(tile_x,tile_y)| {
            let x=tile_x.checked_mul(request.source_tile).ok_or_else(||anyhow::anyhow!("preview x overflow"))?/self.scale;
            let y=tile_y.checked_mul(request.source_tile).ok_or_else(||anyhow::anyhow!("preview y overflow"))?/self.scale;
            anyhow::ensure!(x<self.width && y<self.height,"preview crop outside image");
            let width=request.source_tile.div_ceil(self.scale).min(self.width-x);let height=request.source_tile.div_ceil(self.scale).min(self.height-y);
            let started=std::time::Instant::now();
            let bytes=crate::decoder::encode_crop_rgba(&self.data,self.width,x,y,width,height,request.format)?;
            Ok(DecodedTile {tile_x,tile_y,rgba:None,raster:DecodedRaster {width,height,bytes:bytes.into(),content_type:request.format.content_type(),
                decoder_route:"preview-pixel-reuse",decode_ms:0.0,encode_ms:crate::decoder::elapsed_ms(started),source_cache_status:"decoded-preview-reuse",
                source_cache_ms:0.0,compressed_resolution_view:false,source_read_bytes:0,source_range_requests:0,source_cache_hit_bytes:0}})
        }).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(format:river_core::RasterFormat)->TileDecodeRequest {
        TileDecodeRequest { output_geometry:None, edge_mode:crate::edges::Mode::Crop,source_id:"immutable".into(),source:crate::decoder::Jpeg2000Source::from_path("immutable.jp2",12,6),
            webp_method:0,capture_pixels:false,whole_view:true,tiles:vec![(0,0),(1,0)],source_tile:8,reduction:1,layers:0,format,
            decoder_threads:1,max_native_tiles:4,cached_tiles:vec![],work_ticket:None,cancellation:None}
    }
    fn image()->Vec<u8> {(0..18).flat_map(|n|[n*11,n*7,255-n*9,if n%2==0 {255} else {128}]).collect()}
    fn patches(pixels:&mut Pixels,data:&[u8]) {
        for y in (0..2).rev() {for x in (0..3).rev() {
            let h=2.min(3-y*2);let mut rgba=Vec::new();
            for row in y*2..y*2+h {rgba.extend_from_slice(&data[((row*6+x*2)*4) as usize..((row*6+x*2+2)*4) as usize]);}
            pixels.add(&DecodedTile {tile_x:x,tile_y:y,rgba:Some(rgba.into()),raster:DecodedRaster {width:2,height:h,bytes:vec![].into(),
                content_type:"image/png",decoder_route:"test",decode_ms:0.0,encode_ms:0.0,source_cache_status:"test",source_cache_ms:0.0,
                compressed_resolution_view:false,source_read_bytes:0,source_range_requests:0,source_cache_hit_bytes:0}});
        }}
    }
    #[test]
    fn stitches_original_rgba_with_edges_and_alpha_without_lossy_round_trip() {
        for format in [river_core::RasterFormat::Png,river_core::RasterFormat::Webp] {
            let request=request(format);let data=image();let mut pixels=Pixels::new(&request,4).unwrap();patches(&mut pixels,&data);
            assert!(pixels.complete());assert_eq!(pixels.data,data);
            let output=pixels.encode(&request).unwrap();let _encoding=crate::encoding_settings::Scope::new(0);
            for tile in output {
                let x=tile.tile_x*4;let width=4.min(6-x);
                let reference=crate::decoder::encode_crop_rgba(&data,6,x,0,width,3,format).unwrap();
                assert_eq!(tile.raster.bytes,reference);assert!(tile.rgba.is_none());
            }
        }
    }
    #[test]
    fn missing_raw_pixels_and_unsupported_alignment_fall_back() {
        let request=request(river_core::RasterFormat::Png);let mut pixels=Pixels::new(&request,4).unwrap();assert!(!pixels.complete());
        patches(&mut pixels,&image());assert!(pixels.complete());
        let mut request=request;request.source.reference_origin=(1,1);assert!(Pixels::new(&request,4).is_none());
        request.source.reference_origin=(0,0);request.reduction=3;assert!(Pixels::new(&request,4).is_none());
    }
}
