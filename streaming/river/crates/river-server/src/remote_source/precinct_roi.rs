//! Conservative spatial packet demand. Originals and packet entropy stay intact.
use super::{Header,Packet,ReadContext};
type Rect=(u64,u64,u64,u64);
pub(super) struct Selection {regions:Vec<Rect>,exact:Option<Vec<Vec<Rect>>>}
impl Selection {
 pub(super) fn rebase(&self,h:&Header)->Option<Window> {
  let mode=std::env::var("RIVER_S3_PRECINCT_REBASE").unwrap_or_else(|_|"auto".into());
  self.rebase_with_mode(h,&mode)
 }
 fn rebase_with_mode(&self,h:&Header,mode:&str)->Option<Window> {
  if !matches!(mode,"auto"|"true"|"1") || h.layers!=1 || h.cod[0]&6!=0
   || h.layout.columns!=1 || h.layout.rows!=1 || h.layout.origin_x!=0 || h.layout.origin_y!=0 {return None;}
  let mut ax=1u64<<(h.levels-1);let mut ay=ax;
  for r in 0..h.levels {
   let scale=1u64<<(h.levels-1-r);let pp=if h.cod[0]&1!=0 {h.cod[10+usize::from(r)]}else{0xff};
   ax=ax.max((1u64<<(pp&15))*scale);ay=ay.max((1u64<<(pp>>4))*scale);
  }
  let x0=self.regions.iter().map(|r|r.0).min()? /ax*ax;
  let y0=self.regions.iter().map(|r|r.1).min()? /ay*ay;
  let x1=(self.regions.iter().map(|r|r.2).max()?.div_ceil(ax)*ax).min(u64::from(h.width));
  let y1=(self.regions.iter().map(|r|r.3).max()?.div_ceil(ay)*ay).min(u64::from(h.height));
  if x0==0 && y0==0 && x1==u64::from(h.width) && y1==u64::from(h.height) {return None;}
  let original=u64::from(h.width)*u64::from(h.height);let window=(x1-x0)*(y1-y0);
  // Avoid geometry allocation for a huge native canvas. Alignment preserves
  // every precinct/codeblock and the full-depth reversible support envelope.
  if mode=="auto" && (original<64*1024*1024 || window>256*1024*1024 || window.saturating_mul(4)>original) {return None;}
  Some(Window {x0:x0 as u32,y0:y0 as u32,x1:x1 as u32,y1:y1 as u32})
 }
 pub(super) fn new(h:&Header,tiles:&[(u32,u32)],edge:u32,reduction:u8,context:&mut ReadContext)->Option<Self> {
  if !std::env::var("RIVER_S3_PRECINCT_ROI").is_ok_and(|v|v=="true" || v=="1")
   || !h.packet_compatible || h.is_ht || h.cod[9]!=1 || h.image_origin!=(0,0) {return None;}
  // Spatial support is independent of quality layers. Keep or omit every
  // layer of a precinct together; packet filtering applies the layer limit.
  // Multilayer coordinate rebasing remains separately disabled above.
  // A deliberately conservative full-depth5/3 support envelope. Keeping extra
  // precincts is safe; omitting required wavelet neighbours is not.
  let margin=4*((1u64<<(h.levels-1))-1);
  let regions=tiles.iter().map(|&(x,y)| {
   let x=u64::from(x)*u64::from(edge);let y=u64::from(y)*u64::from(edge);
   (x.saturating_sub(margin),y.saturating_sub(margin),(x+u64::from(edge)+margin).min(u64::from(h.width)),(y+u64::from(edge)+margin).min(u64::from(h.height)))
  }).collect::<Vec<Rect>>();
  let exact_enabled=std::env::var("RIVER_S3_EXACT_SUPPORT").is_ok_and(|v|v=="true" || v=="1")
   && h.layout.columns==1 && h.layout.rows==1 && h.layout.origin_x==0 && h.layout.origin_y==0
   && tiles.len()<=64 && reduction<h.levels
   && !tiles.iter().any(|&(x,y)|x==0 && y==0 && edge>=h.width && edge>=h.height)
   && (1..h.levels).all(|r|{let pp=if h.cod[0]&1!=0 {h.cod[10+usize::from(r)]}else{0xff};pp&15>0 && pp>>4>0});
  let budget=usize::from(h.levels).saturating_mul(tiles.len().saturating_mul(3).saturating_mul(std::mem::size_of::<Rect>())+std::mem::size_of::<Vec<Rect>>());
  let exact=if exact_enabled && context.admit_metadata(budget) {
   let rects=tiles.iter().map(|&(x,y)|{let x=u64::from(x)*u64::from(edge);let y=u64::from(y)*u64::from(edge);(x,y,(x+u64::from(edge)).min(u64::from(h.width)),(y+u64::from(edge)).min(u64::from(h.height)))}).collect::<Vec<_>>();
   let mut plan=exact_support(&rects,u64::from(h.width),u64::from(h.height),h.levels,reduction);
   // Convert once, rather than dividing four coordinates for every packet in
   // every discovery/prefetch/reconstruction pass.
   for (r,bands) in plan.iter_mut().enumerate() {
    let factor=1u64<<(usize::from(h.levels)-1-r+usize::from(r>0));
    for rect in bands {rect.0*=factor;rect.1*=factor;rect.2*=factor;rect.3*=factor;}
   }
   if worthwhile(h,&regions,&plan,reduction) {Some(plan)}else{context.metadata_bytes=context.metadata_bytes.saturating_sub(budget);None}
  }else{None};
  Some(Self {regions,exact})
 }
 pub(super) fn covers(&self,bounds:(u32,u32,u32,u32))->bool {
  // The old full-depth envelope may cover a whole native tile even when
  // the exact per-band demand is tiny. Do not discard that spatial mask.
  if self.exact.is_some() {return false;}
  self.regions.iter().any(|&(x0,y0,x1,y1)|x0<=u64::from(bounds.0) && y0<=u64::from(bounds.1) && x1>=u64::from(bounds.2) && y1>=u64::from(bounds.3))
 }
 pub(super) fn keeps(&self,h:&Header,p:&Packet)->bool {
  let scale=1u64<<(h.levels-1-p.resolution);let pp=if h.cod[0]&1!=0 {h.cod[10+usize::from(p.resolution)]}else{0xff};
  let width=(1u64<<(pp&15))*scale;let height=(1u64<<(pp>>4))*scale;
  let end_x=(p.x/width+1)*width;let end_y=(p.y/height+1)*height;
  if let Some(exact)=&self.exact {
   return exact[usize::from(p.resolution)].iter().any(|&(a,b,c,d)|p.x<c && p.y<d && end_x>a && end_y>b);
  }
  self.regions.iter().any(|&(x0,y0,x1,y1)|p.x<x1 && p.y<y1 && end_x>x0 && end_y>y0)
 }
}
pub(super) struct Window {pub x0:u32,pub y0:u32,x1:u32,y1:u32}
impl Window {
 pub fn width(&self)->u32 {self.x1-self.x0}
 pub fn height(&self)->u32 {self.y1-self.y0}
 pub fn contains(&self,p:&Packet)->bool {p.x>=u64::from(self.x0) && p.y>=u64::from(self.y0) && p.x<u64::from(self.x1) && p.y<u64::from(self.y1)}
 pub fn patch_siz(&self,main:&mut [u8])->anyhow::Result<()> {
  let mut at=2;
  while at+4<=main.len() {
   let marker=super::be16(main,at)?;let length=usize::from(super::be16(main,at+2)?);
   anyhow::ensure!(length>=2 && at+2+length<=main.len(),"invalid technical window main header");
   if marker==0xff51 {
    anyhow::ensure!(length>=38,"truncated technical window SIZ");
    for (index,value) in [self.width(),self.height(),0,0,self.width(),self.height(),0,0].into_iter().enumerate() {
     main[at+6+4*index..at+10+4*index].copy_from_slice(&value.to_be_bytes());
    }
    return Ok(());
   }
   at+=2+length;
  }
  anyhow::bail!("technical window SIZ missing")
 }
}
#[cfg(test)] mod tests {
 use super::*;
 fn header()->Header {Header {image_origin:(0,0),width:4096,height:4096,components:3,bit_depth:8,levels:7,layers:1,layout:crate::decoder::NativeTileLayoutHint {origin_x:0,origin_y:0,width:4096,height:4096,columns:1,rows:1},progression:2,is_ht:false,cod:vec![1,2,0,1,0,6,4,4,0,1,0x22,0x33,0x44,0x55,0x66,0x77,0x88],first_part:0,codestream_end:100,packet_compatible:true,prefix_compatible:true,payload_digest:[0;32]}}
 #[test] fn window_alignment_preserves_support_and_bounds_huge_geometry() {
  let mut h=header();h.width=16384;h.height=16384;h.layout.width=h.width;h.layout.height=h.height;
  let roi=Selection {exact:None,regions:vec![(1796,1796,2812,2812)]};
  let w=roi.rebase_with_mode(&h,"auto").unwrap();
  assert!(w.x0<=1796 && w.y0<=1796 && w.x1>=2812 && w.y1>=2812);
  for r in 0..h.levels {let pp=h.cod[10+usize::from(r)];let scale=1u64<<(h.levels-1-r);
   assert_eq!(u64::from(w.x0)%((1u64<<(pp&15))*scale),0);assert_eq!(u64::from(w.y0)%((1u64<<(pp>>4))*scale),0);
  }
  assert!(u64::from(w.width())*u64::from(w.height())*4<u64::from(h.width)*u64::from(h.height));
  assert!(roi.rebase_with_mode(&h,"false").is_none());
  h.layers=2;h.cod[3]=2;assert!(roi.rebase_with_mode(&h,"true").is_none());
  h.layers=1;h.cod[3]=1;h.cod[0]|=2;assert!(roi.rebase_with_mode(&h,"auto").is_none());
 }
 #[test] fn small_geometry_stays_on_existing_automatic_path() {
  let h=header();let roi=Selection {exact:None,regions:vec![(512,512,1024,1024)]};
  assert!(roi.rebase_with_mode(&h,"auto").is_none());assert!(roi.rebase_with_mode(&h,"true").is_some());
 }
 #[test] fn spatial_mask_uses_reference_grid_at_every_resolution() {
  let h=header();let roi=Selection {exact:None,regions:vec![(260,260,1276,1276)]};
  for resolution in 0..7 {let mut p=Packet {resolution,layer:0,component:0,x:256,y:256};assert!(roi.keeps(&h,&p));p.x=1280;assert!(!roi.keeps(&h,&p));p.x=0;p.y=0;assert!(!roi.keeps(&h,&p));}
  assert!(!roi.covers(h.tile_bounds(0)));
 }
 #[test] fn exact_cost_gate_requires_material_precinct_savings() {
  let h=header();let old=[(0,0,380,380)];
  let small=(0..h.levels).map(|_|vec![(0,0,256,256)]).collect::<Vec<_>>();
  assert!(worthwhile(&h,&old,&small,0));
  let equal=(0..h.levels).map(|_|old.to_vec()).collect::<Vec<_>>();assert!(!worthwhile(&h,&old,&equal,0));
  let mut lowest_only=equal.clone();lowest_only[0]=vec![(0,0,256,256)];assert!(!worthwhile(&h,&old,&lowest_only,0));
  let mut giant=h.clone();giant.width=u32::MAX;giant.height=u32::MAX;assert!(worthwhile(&giant,&[(0,0,u64::from(u32::MAX),u64::from(u32::MAX))],&small,0));
  let mut large=h.clone();large.width=16384;large.height=16384;
  assert!(worthwhile(&large,&[(0,0,16384,16384)],&small,0));
  let selected=Selection {regions:vec![(0,0,4096,4096)],exact:Some(small.clone())};assert!(!selected.covers((0,0,4096,4096)));
  let mut tiny=h.clone();tiny.cod[10..].fill(0x22);assert!(!worthwhile(&tiny,&[(0,0,4096,4096),(0,0,2048,2048)],&small,0));
 }
 #[test] fn sparse_windows_do_not_fetch_intervening_precincts() {let h=header();let roi=Selection {exact:None,regions:vec![(0,0,512,512),(3584,3584,4096,4096)]};let mut p=Packet {resolution:6,layer:0,component:0,x:2048,y:2048};assert!(!roi.keeps(&h,&p));p.x=3840;p.y=3840;assert!(roi.keeps(&h,&p));}
}

// Reverse the reversible5/3 lifting dependencies, rather than applying one
// full-depth margin to every band. LL uses L; detail packets cover HL/LH/HH.
fn exact_support(regions:&[Rect],width:u64,height:u64,levels:u8,reduction:u8)->Vec<Vec<Rect>> {
 let mut bands=(0..levels).map(|_|Vec::with_capacity(regions.len()*3)).collect::<Vec<_>>();
 let scale=1u64<<reduction;
 for &(x0,y0,x1,y1) in regions {
  let mut a=(x0/scale,y0/scale,x1.div_ceil(scale),y1.div_ceil(scale));
  if a.0>=a.2 || a.1>=a.3 {continue;}
  let (mut w,mut h)=(width.div_ceil(scale),height.div_ceil(scale));
  for r in (1..levels-reduction).rev() {
   let low=(a.0/2,a.1/2,(a.2/2+1).min(w.div_ceil(2)),(a.3/2+1).min(h.div_ceil(2)));
   let high=(low.0.saturating_sub(1),low.1.saturating_sub(1),low.2.min(w/2),low.3.min(h/2));
   for rect in [(high.0,low.1,high.2,low.3),(low.0,high.1,low.2,high.3),high] {
    if rect.0<rect.2 && rect.1<rect.3 {bands[usize::from(r)].push(rect);}
   }
   a=low;w=w.div_ceil(2);h=h.div_ceil(2);
  }
  bands[0].push(a);
 }
 bands
}

#[cfg(test)] mod exact_tests {
 use super::*;
 #[test] fn small_region_omits_unrelated_precincts_at_each_resolution() {
  let bands=exact_support(&[(64,64,128,128)],4096,4096,7,2);
  assert!(bands[5].is_empty() && bands[6].is_empty());
  assert_eq!(bands[0],vec![(1,1,3,3)]);
  for rects in &bands[..5] {assert!(!rects.is_empty());for &(x0,y0,x1,y1) in rects {assert!(x0<x1 && y0<y1);}}
 }
 #[test] fn odd_image_boundary_support_is_clipped_without_empty_bands() {
  let bands=exact_support(&[(4094,4092,4095,4093)],4095,4093,7,0);
  for (r,rects) in bands.iter().enumerate() {
   let scale=1u64<<(6-r+usize::from(r>0));
   for &(x0,y0,x1,y1) in rects {assert!(x0<x1 && y0<y1 && x1<=4095u64.div_ceil(scale) && y1<=4093u64.div_ceil(scale));}
  }
 }
}

// Do not pay the per-packet mask cost when rounded precinct demand barely
// changes. Count unique cells with a hard bound on this planning work.
fn worthwhile(h:&Header,old:&[Rect],plan:&[Vec<Rect>],reduction:u8)->bool {
 fn cells(rects:&[Rect],pw:u64,ph:u64,width:u64,height:u64)->Option<usize> {
  let mut cells=std::collections::HashSet::new();
  for &(a,b,c,d) in rects {
   let (x0,y0,x1,y1)=(a.min(width)/pw,b.min(height)/ph,c.min(width).div_ceil(pw),d.min(height).div_ceil(ph));
   if x1<=x0 || y1<=y0 {continue;}
   let area=(x1-x0).checked_mul(y1-y0)?;
   if rects.len()==1 {return usize::try_from(area).ok();}
   if area>512 {return None;}
   for y in y0..y1 {for x in x0..x1 {cells.insert((x,y));if cells.len()>512 {return None;}}}
  }
  Some(cells.len())
 }
 let (mut before,mut after)=(0u128,0u128);
 for r in 0..h.levels-reduction {
  let scale=1u64<<(h.levels-1-r);let pp=if h.cod[0]&1!=0 {h.cod[10+usize::from(r)]}else{0xff};
  let (w,p)=(1u64<<(pp&15),1u64<<(pp>>4));
  let Some(a)=cells(old,w*scale,p*scale,u64::from(h.width),u64::from(h.height))else{return false;};
  let Some(b)=cells(&plan[usize::from(r)],w*scale,p*scale,u64::from(h.width),u64::from(h.height))else{return false;};
  let weight=if r==0 {w*p}else{w*p/4*3};before+=a as u128*u128::from(weight);after+=b as u128*u128::from(weight);
 }
 before>0 && after.saturating_mul(100)<=before.saturating_mul(95)
}
