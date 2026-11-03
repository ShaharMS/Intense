use std::{borrow::Cow,cell::Cell};
use anyhow::{Result,ensure,bail};

#[derive(Clone,Copy,Debug,Default,Eq,PartialEq)]
pub enum Mode { #[default] Crop, Black, Transparent }
impl Mode {
    pub fn parse(value:&str)->Result<Self> {
        match value.to_ascii_lowercase().as_str() {
            ""|"crop"|"none"=>Ok(Self::Crop),"black"=>Ok(Self::Black),"transparent"=>Ok(Self::Transparent),
            _=>bail!("Invalid edge mode. Use crop, black or transparent."),
        }
    }
    pub fn as_str(self)->&'static str {match self {Self::Crop=>"crop",Self::Black=>"black",Self::Transparent=>"transparent"}}
}
#[derive(Clone,Copy,Debug)]
pub struct OutputGeometry {pub edge:u32,pub width:u32,pub height:u32}
thread_local! {static OUTPUT:Cell<(Mode,u32,Option<OutputGeometry>)>=const {Cell::new((Mode::Crop,0,None))};}
pub struct Scope {previous:(Mode,u32,Option<OutputGeometry>)}
impl Scope {
    pub fn new(mode:Mode,source_edge:u32,reduction:u8)->Self {
        Self::with_output(mode,source_edge,reduction,None)
    }
    pub fn with_output(mode:Mode,source_edge:u32,reduction:u8,output:Option<OutputGeometry>)->Self {
        let edge=source_edge.div_ceil(1u32.checked_shl(u32::from(reduction)).unwrap_or(u32::MAX));
        Self {previous:OUTPUT.with(|slot|slot.replace((mode,output.map_or(edge,|g|g.edge),output)))}
    }
}
impl Drop for Scope {fn drop(&mut self) {OUTPUT.with(|slot|slot.set(self.previous));}}

pub fn pad<'a>(rgba:Cow<'a,[u8]>,width:u32,height:u32)->Result<(Cow<'a,[u8]>,u32,u32)> {
    let (mode,edge,output)=OUTPUT.with(|slot|slot.get());
    let (rgba,width,height)=if let Some(g)=output {
        ensure!(g.edge>0 && g.edge<=4096 && g.width>0 && g.height>0 && g.width<=g.edge && g.height<=g.edge,"invalid compatibility output geometry");
        if width==g.width && height==g.height {(rgba,width,height)} else {
            let image=image::RgbaImage::from_raw(width,height,rgba.into_owned()).ok_or_else(||anyhow::anyhow!("invalid decoded RGBA length"))?;
            let scaled=image::imageops::resize(&image,g.width,g.height,image::imageops::FilterType::Triangle);
            (Cow::Owned(scaled.into_raw()),g.width,g.height)
        }
    } else {(rgba,width,height)};
    if mode==Mode::Crop || (width==edge && height==edge) {return Ok((rgba,width,height));}
    ensure!(edge>0 && edge<=4096 && width<=edge && height<=edge,"decoded edge tile exceeds its nominal dimensions");
    ensure!(rgba.len()==width as usize*height as usize*4,"invalid RGBA edge tile length");
    let mut padded=vec![0;edge as usize*edge as usize*4];
    if mode==Mode::Black {for pixel in padded.chunks_exact_mut(4) {pixel[3]=255;}}
    for y in 0..height as usize {
        let row=width as usize*4;let dest=y*edge as usize*4;
        padded[dest..dest+row].copy_from_slice(&rgba[y*row..(y+1)*row]);
    }
    Ok((Cow::Owned(padded),edge,edge))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn padding_preserves_pixels_alpha_and_scope_between_jobs() {
        let pixels=[3,5,7,91,11,13,17,255];
        for (mode,alpha) in [(Mode::Black,255),(Mode::Transparent,0)] {
            let _scope=Scope::new(mode,4,0);let (out,w,h)=pad(Cow::Borrowed(&pixels),2,1).unwrap();
            assert_eq!((w,h),(4,4));assert_eq!(&out[..8],&pixels);
            for pixel in out[8..].chunks_exact(4) {assert_eq!(pixel,[0,0,0,alpha]);}
        }
        let (out,w,h)=pad(Cow::Borrowed(&pixels),2,1).unwrap();assert!(matches!(out,Cow::Borrowed(_)));assert_eq!((w,h),(2,1));
        let _scope=Scope::new(Mode::Black,512,2);assert_eq!(pad(Cow::Borrowed(&pixels),2,1).unwrap().1,128);
    }
}

#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn resamples_before_padding_and_restores_scope() {
        let pixels=[71,83,97,255].repeat(6);
        for mode in [Mode::Crop,Mode::Black,Mode::Transparent] {
            let _scope=Scope::with_output(mode,1024,2,Some(OutputGeometry {edge:768,width:384,height:256}));
            let (out,w,h)=pad(Cow::Borrowed(&pixels),3,2).unwrap();
            assert_eq!((w,h),if mode==Mode::Crop {(384,256)}else{(768,768)});
            assert_eq!(&out[..4],&[71,83,97,255]);
            let last=&out[out.len()-4..];assert_eq!(last,if mode==Mode::Crop {&[71,83,97,255]}else if mode==Mode::Black {&[0,0,0,255]}else{&[0,0,0,0]});
        }
        assert_eq!(pad(Cow::Borrowed(&pixels),3,2).unwrap().1,3);
    }
}
