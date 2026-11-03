use std::{ffi::{c_void, CString}, sync::OnceLock};
use anyhow::{Context, Result, ensure};
use super::super::Jpeg2000Source;

#[repr(C)]
#[derive(Default)]
struct Plane {
    data: *const c_void,
    width: u32, height: u32, stride: u32, x: u32, y: u32,
    precision: u8, signed_samples: u8, sample_bytes: u8,
}
type Decode = unsafe extern "C" fn(*const i8, *const u8, usize, u32, u32, u32, u32, u8, u16) -> *mut c_void;
struct Api {
    _library: libloading::Library,
    decode: Decode,
    release: unsafe extern "C" fn(*mut c_void),
    components: unsafe extern "C" fn(*const c_void) -> u16,
    plane: unsafe extern "C" fn(*const c_void, u16, *mut Plane) -> bool,
}
impl Api {
    unsafe fn load() -> Result<Self> {
        unsafe {
        let path = std::env::var("RIVER_GROK_NATIVE_LIBRARY").unwrap_or_else(|_| "/usr/local/lib/libriver_grok.so".into());
        let library = libloading::Library::new(path).context("native Grok bridge unavailable")?;
        let initialize = *library.get::<unsafe extern "C" fn(u32)>(b"river_grok_initialize\0")?;
        let api = Self { decode: *library.get(b"river_grok_decode\0")?, release: *library.get(b"river_grok_release\0")?,
            components: *library.get(b"river_grok_components\0")?, plane: *library.get(b"river_grok_plane\0")?, _library: library };
        let threads = std::env::var("RIVER_GROK_NATIVE_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(2u32).clamp(1, 64);
        initialize(threads);
        Ok(api)
        }
    }
}
static API: OnceLock<Result<Api, String>> = OnceLock::new();
struct Handle<'a> { ptr: *mut c_void, api: &'a Api }
impl Drop for Handle<'_> { fn drop(&mut self) { unsafe { (self.api.release)(self.ptr) } } }

pub(super) fn decode(source: &Jpeg2000Source, region: (u32,u32,u32,u32), reduction: u8, layers: u32, prefer_openht:bool) -> Result<(image::RgbaImage,&'static str)> {
    if prefer_openht {
        if let Some(image)=source.prepared.as_ref().and_then(|info|info.predecoded.as_ref()) {
            let scale=1u32<<reduction;let (x0,y0,x1,y1)=region;
            let origin=source.prepared.as_ref().unwrap().predecoded_origin;
            let x=(x0/scale).checked_sub(origin.0).context("pipeline crop precedes decoded columns")?;
            let y=(y0/scale).checked_sub(origin.1).context("pipeline crop precedes decoded rows")?;
            let (w,h)=(x1.div_ceil(scale)-x0/scale,y1.div_ceil(scale)-y0/scale);
            ensure!(x+w<=image.width() && y+h<=image.height(),"pipeline output region exceeds decoded image");
            return Ok((image::imageops::crop_imm(image.as_ref(),x,y,w,h).to_image(),"openht-pipeline"));
        }
    }
    let minimum=std::env::var("RIVER_OPENHT_MIN_PACKETS").ok().and_then(|v|v.parse().ok());
    let enabled=prefer_openht || std::env::var("RIVER_GROK_PACKED_VIEWS").is_ok_and(|v|v=="true" || v=="1");
    let view=if enabled {source.memory.as_ref().and_then(|data|super::packed::build(data,reduction).or_else(||if prefer_openht && std::env::var("RIVER_OPENHT_COARSE").is_ok_and(|v|v=="true" || v=="1") {super::packed::coarse(data,reduction,Some(region),minimum)}else{None}))}else{None};
    if std::env::var("RIVER_PROFILE_SOURCE").is_ok_and(|v|v=="true") {
        if let Some(view)=&view {tracing::info!(width=view.width,height=view.height,packed_bytes=view.data.len(),"Grok packed LL view selected");}
    }
    #[cfg(unix)]
    if prefer_openht {
        let direct_enabled=!std::env::var("RIVER_OPENHT_DIRECT_BLOCKS").is_ok_and(|v|v=="false" || v=="0");
        if let Some(view)=view.as_ref().filter(|view|if view.coarse {super::super::openht_policy::prefer_coarse(view.packets,view.width,view.height,region,minimum)}else{super::super::openht_policy::prefer(view.packets,view.width,view.height,view.direct_compatible,minimum,direct_enabled)}) {
            let (x0,y0,x1,y1)=region;
            if x0%view.scale==0 && y0%view.scale==0 && x1.div_ceil(view.scale)<=view.width && y1.div_ceil(view.scale)<=view.height {
                let image=super::openht_worker::decode(&view.data,view.width,view.height)
                    .map_err(|error|anyhow::Error::new(super::openht_worker::Failure(error)))?;
                let area=(x0/view.scale,y0/view.scale,x1.div_ceil(view.scale),y1.div_ceil(view.scale));
                let image=if area==(0,0,view.width,view.height) {image} else {image::imageops::crop_imm(&image,area.0,area.1,area.2-area.0,area.3-area.1).to_image()};
                return Ok((image,if view.coarse {"openht-coarse"}else{"openht-packed"}));
            }
        }
    }
    let api = API.get_or_init(|| unsafe { Api::load().map_err(|error| format!("{error:#}")) });
    let api = api.as_ref().map_err(|error| anyhow::anyhow!("{error}"))?;
    let path = CString::new(source.path.to_string_lossy().as_bytes())?;
    let (bytes,length,area,level)=if let Some(view)=&view {
        let (x0,y0,x1,y1)=region;
        if x0%view.scale==0 && y0%view.scale==0 && x1.div_ceil(view.scale)<=view.width && y1.div_ceil(view.scale)<=view.height {
            (view.data.as_ptr(),view.data.len(),(x0/view.scale,y0/view.scale,x1.div_ceil(view.scale),y1.div_ceil(view.scale)),0)
        } else {let (b,n)=source.memory.as_ref().map(|d|(d.as_ptr(),d.len())).unwrap_or((std::ptr::null(),0));(b,n,region,reduction)}
    } else {let (b,n)=source.memory.as_ref().map(|d|(d.as_ptr(),d.len())).unwrap_or((std::ptr::null(),0));(b,n,region,reduction)};
    let (x0,y0,x1,y1)=area;
    let ptr = unsafe { (api.decode)(path.as_ptr(),bytes,length,x0,y0,x1,y1,level,u16::try_from(layers)?) };
    ensure!(!ptr.is_null(), "native Grok decode failed");
    let handle = Handle { ptr, api };
    let components = unsafe { (api.components)(handle.ptr) };
    ensure!((1..=4).contains(&components), "unsupported native Grok components");
    let mut planes = Vec::new();
    for component in 0..components {
        let mut plane = Plane::default();
        ensure!(unsafe { (api.plane)(handle.ptr,component,&mut plane) }, "unsupported native Grok plane");
        ensure!(!plane.data.is_null() && (1..=16).contains(&plane.precision) && plane.stride >= plane.width
            && matches!(plane.sample_bytes,2|4), "invalid native Grok plane");
        if let Some(first) = planes.first() {
            let first: &Plane = first;
            ensure!((plane.width,plane.height,plane.x,plane.y)==(first.width,first.height,first.x,first.y), "native Grok plane geometry mismatch");
        }
        planes.push(plane);
    }
    let width = planes[0].width; let height = planes[0].height;
    let pixels = u64::from(width)*u64::from(height);
    ensure!(pixels > 0 && pixels <= 64*1024*1024, "native Grok raster exceeds memory limit");
    let mut rgba = Vec::with_capacity(pixels as usize*4);
    for y in 0..height { for x in 0..width {
        let mut values = [0u8;4];
        for (component, plane) in planes.iter().enumerate() {
            let index = y as usize*plane.stride as usize+x as usize;
            let value = unsafe { if plane.sample_bytes==2 { *(plane.data as *const i16).add(index) as i32 }
                else { *(plane.data as *const i32).add(index) } };
            let value = value + if plane.signed_samples!=0 { 1 << (plane.precision-1) } else { 0 };
            let max = (1i32 << plane.precision)-1;
            values[component] = ((value.clamp(0,max) as u32*255 + max as u32/2)/max as u32) as u8;
        }
        let pixel = match components {
            1 => [values[0],values[0],values[0],255],
            2 => [values[0],values[0],values[0],values[1]],
            3 => [values[0],values[1],values[2],255],
            _ => values,
        };
        rgba.extend_from_slice(&pixel);
    } }
    image::RgbaImage::from_raw(width,height,rgba).context("native Grok raster size mismatch").map(|image|(image,"jp2grok"))
}
