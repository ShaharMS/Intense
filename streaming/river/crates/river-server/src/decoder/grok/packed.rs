//! Conservative, entropy-preserving LL packing. Unknown layouts return None.
use std::ops::Range;
pub(super) struct View {pub data:Vec<u8>,pub scale:u32,pub width:u32,pub height:u32,pub packets:usize,pub direct_compatible:bool,pub coarse:bool}
fn u16_at(b:&[u8],p:usize)->Option<u16> {Some(u16::from_be_bytes(b.get(p..p+2)?.try_into().ok()?))}
fn u32_at(b:&[u8],p:usize)->Option<u32> {Some(u32::from_be_bytes(b.get(p..p+4)?.try_into().ok()?))}
fn marker(out:&mut Vec<u8>,code:u16,payload:&[u8])->Option<()> {out.extend(code.to_be_bytes());out.extend(u16::try_from(payload.len()+2).ok()?.to_be_bytes());out.extend(payload);Some(())}
fn stream(b:&[u8])->Option<(&[u8],Option<(u32,u32,u16,u8)>)> {
 if b.starts_with(&[255,79]) {return Some((b,None));}
 let mut p=0usize;let mut declared=None;let mut color=None;
 while p+8<=b.len() {
  let size=u32_at(b,p)? as usize;let kind=b.get(p+4..p+8)?;let (size,head)=if size==1 {(usize::try_from(u64::from_be_bytes(b.get(p+8..p+16)?.try_into().ok()?)).ok()?,16)} else if size==0 {(b.len()-p,8)} else {(size,8)};
  let end=p.checked_add(size)?;if size<head || end>b.len() {return None;}
  if kind==b"jp2h" {
   let mut q=p+8;
   while q<end {
    let n=u32_at(b,q)? as usize;let name=b.get(q+4..q+8)?;if n<8 || q.checked_add(n)?>end {return None;}
    if name==b"colr" {
        if n!=15 || b[q+8]!=1 {return None;}
        let value=u32_at(b,q+11)?;if !matches!(value,16|17) || color.is_some_and(|prior|prior!=value) {return None;}color=Some(value);
    } else if name==b"ihdr" {
        if n!=22 || declared.is_some() {return None;}
        declared=Some((u32_at(b,q+12)?,u32_at(b,q+8)?,u16_at(b,q+16)?,b[q+18]));
    } else {return None;}
    q+=n;
   }
  }
  if kind==b"jp2c" {if declared.is_none() {return None;}return Some((b.get(p+head..end)?,declared));}
  p=end;
 }
 None
}
struct Bits<'a> {data:&'a[u8],pos:usize,remaining:u8,byte:u8,previous:u8}
impl Bits<'_> {
 fn bit(&mut self)->Option<u8> {if self.remaining==0 {self.byte=*self.data.get(self.pos)?;self.pos+=1;self.remaining=if self.previous==255 {7}else{8};self.previous=self.byte;}self.remaining-=1;Some((self.byte>>self.remaining)&1)}
 fn bits(&mut self,n:u32)->Option<u64> {if n>32 {return None;}let mut v=0;for _ in 0..n {v=v*2+u64::from(self.bit()?);}Some(v)}
}
fn packet_extent(data:&[u8],start:usize)->Option<(usize,usize)> {
 let mut b=Bits {data,pos:start,remaining:0,byte:0,previous:0};let mut length=0;
 if b.bit()?!=0 && b.bit()?!=0 {
  let mut zeros=0;while b.bit()?==0 {zeros+=1;if zeros>31 {return None;}}
  let passes=if b.bit()?==0 {1} else if b.bit()?==0 {2} else {let x=b.bits(2)?;if x<3 {3+x}else{let x=b.bits(5)?;if x<31 {6+x}else{37+b.bits(7)?}}};
  let mut lblock=3;while b.bit()?!=0 {lblock+=1;if lblock>31 {return None;}}
  length=b.bits(lblock+63-passes.leading_zeros())?;
 }
 let header=b.pos.checked_add(usize::from(b.previous==255))?;let end=header.checked_add(usize::try_from(length).ok()?)?;if end>data.len() {None}else{Some((header,end))}
}
pub(super) fn build(input:&[u8],reduce:u8)->Option<View> {
 if input.len()>66*1024*1024 || !(0..=15).contains(&reduce) {return None;}
 let (b,declared)=stream(input)?;if !b.starts_with(&[255,79]) {return None;}
 let mut p=2;let mut headers:Vec<(u16,Range<usize>)>=Vec::new();let (mut siz,mut cod,mut qcd)=(None,None,None);
 while u16_at(b,p)?!=0xff90 {
  if p>1024*1024 || headers.len()>1024 {return None;}
  let code=u16_at(b,p)?;let n=u16_at(b,p+2)? as usize;if n<2 {return None;}let end=p.checked_add(n+2)?;let payload=b.get(p+4..end)?;
  match code {0xff51=>{if siz.is_some(){return None;}siz=Some(payload)},0xff52=>{if cod.is_some(){return None;}cod=Some(payload)},0xff5c=>{if qcd.is_some(){return None;}qcd=Some(payload)},0xff64=>{},_=>return None}
  headers.push((code,p+4..end));p=end;
 }
 let (siz,cod,qcd)=(siz?,cod?,qcd?);
 if cod.len()!=11+usize::from(reduce) || cod[0]& !6!=1 || cod[1]!=2 || u16_at(cod,2)?!=1 || cod[4]!=0 || cod[5]!=reduce || cod[8]!=0 || cod[9]!=1 || qcd.len()!=2+3*usize::from(reduce) || qcd[0]&31!=0 {return None;}
 if cod[6]>8 || cod[7]>8 || u16::from(cod[6])+u16::from(cod[7])>8 {return None;}
 let scale=1u32<<reduce;let w=u32_at(siz,2)?;let h=u32_at(siz,6)?;let tw=u32_at(siz,18)?;let th=u32_at(siz,22)?;let comps=u16_at(siz,34)? as u32;
 if !matches!(u16_at(siz,0)?,0..=2) || w==0 || h==0 || tw==0 || th==0 || !(1..=4).contains(&comps) || siz.len()!=36+comps as usize*3 || tw%scale!=0 || th%scale!=0 {return None;}
 if let Some((iw,ih,ic,bpc))=declared {if (iw,ih,u32::from(ic))!=(w,h,comps) || (0..comps as usize).any(|c|siz[36+c*3]!=bpc) {return None;}}
 for offset in [10,14,26,30] {if u32_at(siz,offset)?!=0 {return None;}}
 for c in 0..comps as usize {if siz[37+c*3..39+c*3]!=[1,1] || (siz[36+c*3]&127)>15 || siz[36+c*3]&128!=0 {return None;}}
 let (lw,lh)=(tw/scale,th/scale);
 let (pw,ph)=(1u32<<(cod[10]&15),1u32<<(cod[10]>>4));let (cbw,cbh)=(1u32.checked_shl(u32::from(cod[6])+2)?,1u32.checked_shl(u32::from(cod[7])+2)?);
 if pw.min(lw)>cbw || ph.min(lh)>cbh {return None;}
 let (columns,rows)=(w.div_ceil(tw),h.div_ceil(th));let count=columns.checked_mul(rows)?;if !(1..=16384).contains(&count) {return None;}
 let (packed_w,packed_h)=if count==1 {(pw.min(cbw),ph.min(cbh))}else{(pw.min(lw).min(cbw),ph.min(lh).min(cbh))};
 if packed_w==0 || packed_h==0 || !packed_w.is_power_of_two() || !packed_h.is_power_of_two()
    || (count>1 && (lw%packed_w!=0 || lh%packed_h!=0)) {return None;}
 let (width,height)=(w.div_ceil(scale),h.div_ceil(scale));if u64::from(width)*u64::from(height)>4*1024*1024 {return None;}
 let pc=width.div_ceil(packed_w);let pr=height.div_ceil(packed_h);let packets=pc.checked_mul(pr)?.checked_mul(comps)? as usize;if packets>524288 {return None;}
 let mut spans:Vec<Option<(u32,u32,u32)>>=vec![None;packets];let mut seen=vec![false;count as usize];
 for _ in 0..count {
  if u16_at(b,p)?!=0xff90 || u16_at(b,p+2)?!=10 || u16_at(b,p+12)?!=0xff93 {return None;}
  let tile=u16_at(b,p+4)? as u32;let size=u32_at(b,p+6)? as usize;let end=p.checked_add(size)?;
  if tile>=count || seen[tile as usize] || size<14 || end>b.len() || b[p+10]!=0 || b[p+11]>1 {return None;}seen[tile as usize]=true;
  let (x,y)=(tile%columns*tw,tile/columns*th);let (xlow,ylow)=(x/scale,y/scale);let (cw,ch)=((x.saturating_add(tw).min(w)).div_ceil(scale)-xlow,(y.saturating_add(th).min(h)).div_ceil(scale)-ylow);let mut cursor=p+14;
  for py in 0..ch.div_ceil(ph) {for px in 0..cw.div_ceil(pw) {for c in 0..comps {
   let mut first=cursor;
   if cod[0]&2!=0 && b.get(first..first+2)?==[255,145] {if b.get(first+2..first+4)?!=[0,4] || first+6>end {return None;}first+=6;}
   let (header,plain_end)=packet_extent(b.get(..end)?,first)?;
   let eph=usize::from(cod[0]&4!=0)*2;let next=plain_end.checked_add(eph)?;if next>end {return None;}
   if eph!=0 && b.get(header..header+2)?!=[255,146] {return None;}
   let gx=(xlow+px*pw)/packed_w;let gy=(ylow+py*ph)/packed_h;let index=((gy*pc+gx)*comps+c) as usize;let slot=spans.get_mut(index)?;if slot.is_some() {return None;}*slot=Some((u32::try_from(first).ok()?,u32::try_from(header).ok()?,u32::try_from(next).ok()?));cursor=next;
  }}}
  p=end;
 }
 if u16_at(b,p)?!=0xffd9 || p+2!=b.len() || spans.iter().any(Option::is_none) {return None;}
 let mut low_siz=siz.to_vec();low_siz[0..2].copy_from_slice(&0u16.to_be_bytes());for (offset,value) in [(2,width),(6,height),(18,width),(22,height)] {low_siz[offset..offset+4].copy_from_slice(&value.to_be_bytes());}
 let mut low_cod=cod[..10].to_vec();low_cod[0]=1;low_cod[5]=0;low_cod[6]=(packed_w.ilog2().max(2)-2) as u8;low_cod[7]=(packed_h.ilog2().max(2)-2) as u8;low_cod.push(((packed_h.ilog2()<<4)|packed_w.ilog2()) as u8);
 let mut data=vec![255,79];for (code,range) in headers {marker(&mut data,code,if code==0xff51 {&low_siz}else if code==0xff52 {&low_cod}else if code==0xff5c {&qcd[..2]}else{b.get(range)?})?;}
 let eph=usize::from(cod[0]&4!=0)*2;let length=spans.iter().try_fold(14usize,|n,r|{let (start,_,end)=r.as_ref()?;n.checked_add((*end as usize).checked_sub(*start as usize)?.checked_sub(eph)?)})?;data.extend([255,144,0,10,0,0]);data.extend(u32::try_from(length).ok()?.to_be_bytes());data.extend([0,1,255,147]);for span in spans {let (start,header,end)=span?;data.extend(b.get(start as usize..header as usize)?);data.extend(b.get(header as usize+eph..end as usize)?);}data.extend([255,217]);Some(View {data,scale,width,height,packets,coarse:false,direct_compatible:packed_w<=64 && packed_h<=64
    && (0..comps as usize).all(|c|siz[36+c*3]==siz[36])
    && (1..=31).contains(&(u16::from(qcd[1]>>3)+u16::from(qcd[0]>>5)))})
}

#[cfg(test)]
mod tests {
 use super::*;
 fn fixture()->Vec<u8> {
  let mut b=vec![255,79];let mut siz=vec![0,0];for v in [64u32,64,0,0,16,16,0,0] {siz.extend(v.to_be_bytes());}siz.extend([0,1,7,1,1]);marker(&mut b,0xff51,&siz).unwrap();
  marker(&mut b,0xff52,&[1,2,0,1,0,1,4,4,0,1,0x33,0x44]).unwrap();marker(&mut b,0xff5c,&[0x40,0x40,0x48,0x48,0x50]).unwrap();
  for tile in 0..16u16 {b.extend([255,144,0,10]);b.extend(tile.to_be_bytes());b.extend(16u32.to_be_bytes());b.extend([0,1,255,147,0,0]);}b.extend([255,217]);b
 }
 fn boxed(payload:&[u8],kind:&[u8;4])->Vec<u8> {let mut b=((payload.len()+8) as u32).to_be_bytes().to_vec();b.extend(kind);b.extend(payload);b}
 fn jp2(raw:&[u8],width:u32,color:u32)->Vec<u8> {
  let mut ihdr=64u32.to_be_bytes().to_vec();ihdr.extend(width.to_be_bytes());ihdr.extend([0,1,7,7,0,0]);let mut head=boxed(&ihdr,b"ihdr");let mut colr=vec![1,0,0];colr.extend(color.to_be_bytes());head.extend(boxed(&colr,b"colr"));let mut b=boxed(&[13,10,135,10],b"jP  ");b.extend(boxed(&head,b"jp2h"));b.extend(boxed(raw,b"jp2c"));b
 }
 fn marked(flags:u8,sop:bool)->Vec<u8> {
  let b=fixture();let first=b.windows(4).position(|v|v==[255,144,0,10]).unwrap();let mut out=b[..first].to_vec();let cod=out.windows(2).position(|v|v==[255,82]).unwrap();out[cod+4]|=flags;
  for tile in 0..16u16 {
   let mut packets=vec![];for ordinal in 0..2u16 {if sop && flags&2!=0 {packets.extend([255,145,0,4]);packets.extend(ordinal.to_be_bytes());}packets.push(0);if flags&4!=0 {packets.extend([255,146]);}}
   out.extend([255,144,0,10]);out.extend(tile.to_be_bytes());out.extend((14u32+packets.len() as u32).to_be_bytes());out.extend([0,1,255,147]);out.extend(packets);
  }out.extend([255,217]);out
 }
 #[test] fn marked_ll_packets_pack_identically_and_reject_bad_envelopes() {
  let plain=build(&fixture(),1).unwrap().data;
  for flags in [2,4,6] {for sop in [false,true] {
   let b=marked(flags,sop);let original=b.clone();assert_eq!(build(&b,1).unwrap().data,plain);assert_eq!(b,original);
   for end in 0..b.len() {assert!(build(&b[..end],1).is_none());}
  }}
  let mut bad=marked(6,true);let first=bad.windows(4).position(|v|v==[255,144,0,10]).unwrap();bad[first+14+3]=5;assert!(build(&bad,1).is_none());
  let mut bad=marked(6,true);bad[first+14+8]=147;assert!(build(&bad,1).is_none());
 }
 #[test] fn tiny_precincts_keep_legal_nominal_codeblocks() {
  let mut b=fixture();let first=b.windows(4).position(|v|v==[255,144,0,10]).unwrap();b.truncate(first);
  let cod=b.windows(2).position(|v|v==[255,82]).unwrap();b[cod+14]=0x11;b[cod+15]=0x22;
  for tile in 0..16u16 {b.extend([255,144,0,10]);b.extend(tile.to_be_bytes());b.extend(46u32.to_be_bytes());b.extend([0,1,255,147]);b.extend([0;32]);}b.extend([255,217]);
  let view=build(&b,1).unwrap();let cod=view.data.windows(2).position(|v|v==[255,82]).unwrap();assert_eq!(view.data[cod+10],0);assert_eq!(view.data[cod+11],0);assert_eq!(view.data[cod+14],0x11);
 }
 #[test] fn complete_empty_grid_packs_without_modifying_input() {
  let b=fixture();let original=b.clone();let view=build(&b,1).unwrap();assert_eq!((view.width,view.height,view.scale),(32,32,2));assert_eq!(b,original);assert!(view.data.len()<b.len());
  let header=stream(&view.data).unwrap().0;assert!(header.ends_with(&[255,217]));assert_eq!(build(&view.data,0).unwrap().data,view.data);assert!(build(&jp2(&b,64,17),1).is_some());
 }
 #[test] fn one_native_single_resolution_view_rejects_every_truncation() {
  let view=build(&fixture(),1).unwrap();
  for end in 0..view.data.len() {assert!(build(&view.data[..end],0).is_none(),"cut {end}");}
  let repacked=build(&view.data,0).unwrap();assert_eq!((repacked.width,repacked.height,repacked.scale),(view.width,view.height,1));
 }
 #[test] fn clipped_single_and_precinct_aligned_non_power_native_grids_pack_exactly() {
  for (w,h,tw,th) in [(33u32,25u32,33u32,25u32),(60,24,12,12)] {
   let mut b=vec![255,79];let mut siz=vec![0,0];for n in [w,h,0,0,tw,th,0,0] {siz.extend(n.to_be_bytes());}siz.extend([0,1,7,1,1]);marker(&mut b,0xff51,&siz).unwrap();
   marker(&mut b,0xff52,&[1,2,0,1,0,0,0,0,0,1,0x22]).unwrap();marker(&mut b,0xff5c,&[0x40,0x40]).unwrap();
   let columns=w.div_ceil(tw);let rows=h.div_ceil(th);
   for tile in 0..columns*rows {
    let cw=tw.min(w-tile%columns*tw);let ch=th.min(h-tile/columns*th);let packets=cw.div_ceil(4)*ch.div_ceil(4);
    b.extend([255,144,0,10]);b.extend((tile as u16).to_be_bytes());b.extend((14+packets).to_be_bytes());b.extend([0,1,255,147]);b.extend(vec![0;packets as usize]);
   }b.extend([255,217]);
   let view=build(&b,0).unwrap();assert_eq!((view.width,view.height,view.packets),(w,h,(w.div_ceil(4)*h.div_ceil(4)) as usize));
   assert_eq!(build(&view.data,0).unwrap().data,view.data);
   for end in 0..b.len() {assert!(build(&b[..end],0).is_none(),"cut {end}");}
  }
 }
 #[test] fn every_truncation_and_duplicate_tile_falls_back() {
  let b=fixture();for end in 0..b.len() {assert!(build(&b[..end],1).is_none(),"cut {end}");}
  let mut bad=b.clone();let first=bad.windows(4).position(|v|v==[255,144,0,10]).unwrap();bad[first+16+4..first+16+6].copy_from_slice(&0u16.to_be_bytes());assert!(build(&bad,1).is_none());
 }
 #[test] fn unsafe_color_and_mismatched_container_geometry_fall_back() {
  let b=fixture();assert!(build(&jp2(&b,63,17),1).is_none());assert!(build(&jp2(&b,64,18),1).is_none());
  let mut bad=b.clone();let cod=bad.windows(2).position(|v|v==[255,82]).unwrap();bad[cod+8]=1;assert!(build(&bad,1).is_none());
  let mut bad=b.clone();let siz=bad.windows(2).position(|v|v==[255,81]).unwrap();bad[siz+14..siz+18].copy_from_slice(&1u32.to_be_bytes());assert!(build(&bad,1).is_none());
 }
}

// Preserve an already compact zero-DWT codestream. Unlike packed::build this
// path permits several codeblocks per precinct and the reversible RGB transform.
pub(super) fn coarse(input:&[u8],reduce:u8,region:Option<(u32,u32,u32,u32)>,minimum:Option<usize>)->Option<View> {
 if reduce!=0 || input.len()>66*1024*1024 {return None;}
 let (b,declared)=stream(input)?;let mut at=2;let mut main=vec![255,79];
 let (mut siz,mut cod,mut qcd)=(None,None,None);let mut qcc=Vec::new();
 while u16_at(b,at)?!=0xff90 {
  if at>1024*1024 {return None;}
  let code=u16_at(b,at)?;let n=usize::from(u16_at(b,at+2)?);if n<2 {return None;}
  let value=b.get(at+4..at.checked_add(n+2)?)?;
  match code {
   0xff51=>{if siz.replace(value).is_some(){return None;}},
   0xff52=>{if cod.replace(value).is_some(){return None;}},
   0xff5c=>{if qcd.replace(value).is_some(){return None;}},
   0xff5d=>{if value.len()!=3 || value[1]&31!=0 || qcc.contains(&value[0]) {return None;}qcc.push(value[0]);},
   0xff64=>{},_=>return None,
  }
  if code!=0xff64 {main.extend_from_slice(b.get(at..at+n+2)?);}
  at+=n+2;
 }
 let (siz,cod,qcd)=(siz?,cod?,qcd?);
 if cod.len()!=11 || cod[0]!=1 || cod[1]!=2 || u16_at(cod,2)?!=1 || cod[4]>1 || cod[5]!=0
  || cod[6]>4 || cod[7]>4 || cod[8]!=0 || cod[9]!=1 || qcd.len()!=2 || qcd[0]&31!=0 {return None;}
 let width=u32_at(siz,2)?;let height=u32_at(siz,6)?;let components=u16_at(siz,34)?;
 if width==0 || height==0 || u64::from(width)*u64::from(height)>4*1024*1024
  || !(1..=4).contains(&components) || siz.len()!=36+usize::from(components)*3
  || u32_at(siz,18)?<width || u32_at(siz,22)?<height || u16_at(siz,0)?>2 {return None;}
 for offset in [10,14,26,30] {if u32_at(siz,offset)?!=0 {return None;}}
 for c in 0..usize::from(components) {if siz[37+c*3..39+c*3]!=[1,1] || siz[36+c*3]&128!=0 || siz[36+c*3]>15 {return None;}}
 if qcc.iter().any(|&c|u16::from(c)>=components) || (cod[4]==1 && (components!=3 || siz[36]!=siz[39] || siz[36]!=siz[42])) {return None;}
 if let Some((w,h,c,bpc))=declared {if (w,h,c)!=(width,height,components) || (0..usize::from(c)).any(|i|siz[36+3*i]!=bpc) {return None;}}
 let pw=1u32<<(cod[10]&15);let ph=1u32<<(cod[10]>>4);
 let packets=usize::try_from(u64::from(width.div_ceil(pw))*u64::from(height.div_ceil(ph))*u64::from(components)).ok()?;
 if region.is_some_and(|area|!crate::decoder::openht_policy::prefer_coarse(packets,width,height,area,minimum)) {return None;}
 if packets>524288 || u16_at(b,at+2)?!=10 || u16_at(b,at+4)?!=0 || b.get(at+10..at+12)?!=[0,1] {return None;}
 let end=at.checked_add(usize::try_from(u32_at(b,at+6)?).ok()?)?;
 if end.checked_add(2)?!=b.len() || u16_at(b,end)?!=0xffd9 {return None;}
 let mut cursor=at+12;
 while u16_at(b,cursor)?!=0xff93 {
  let code=u16_at(b,cursor)?;let n=usize::from(u16_at(b,cursor+2)?);
  if n<2 || cursor.checked_add(n+2)?>end || !matches!(code,0xff58|0xff64) {return None;}
  cursor+=n+2;
 }
 let body=b.get(cursor+2..end)?;let length=u32::try_from(14+body.len()).ok()?;
 main.extend([255,144,0,10,0,0]);main.extend(length.to_be_bytes());main.extend([0,1,255,147]);main.extend(body);main.extend([255,217]);
 Some(View {data:main,scale:1,width,height,packets,direct_compatible:false,coarse:true})
}

#[cfg(test)] mod coarse_tests {
 use super::*;
 fn fixture()->Vec<u8> {
  let mut b=vec![255,79];let mut siz=vec![0,0];
  for v in [512u32,512,0,0,512,512,0,0] {siz.extend(v.to_be_bytes());}
  siz.extend([0,3,7,1,1,7,1,1,7,1,1]);marker(&mut b,0xff51,&siz).unwrap();
  marker(&mut b,0xff52,&[1,2,0,1,1,0,4,4,0,1,0x88]).unwrap();
  marker(&mut b,0xff5c,&[0x40,0x40]).unwrap();marker(&mut b,0xff5d,&[1,0x40,0x48]).unwrap();
  b.extend([255,144,0,10,0,0]);b.extend(26u32.to_be_bytes());b.extend([0,1,255,147]);b.extend([0;12]);b.extend([255,217]);b
 }
 #[test] fn coarse_rct_multi_block_envelope_is_bounded_and_immutable() {
  let b=fixture();let original=b.clone();let view=coarse(&b,0,None,None).unwrap();
  assert_eq!((view.width,view.height,view.packets),(512,512,12));assert!(view.coarse);assert!(!view.direct_compatible);assert_eq!(view.data,b);assert_eq!(b,original);
  assert!(coarse(&b,0,Some((0,0,64,64)),None).is_none());assert!(coarse(&b,0,Some((0,0,64,64)),Some(0)).is_some());
  assert!(coarse(&b,1,None,None).is_none());assert!(build(&b,0).is_none());
  for end in 0..b.len() {assert!(coarse(&b[..end],0,None,None).is_none());}
  let cod=b.windows(2).position(|p|p==[255,82]).unwrap();
  for (offset,value) in [(0,0),(4,2),(5,1),(6,5),(8,1),(9,0)] {let mut invalid=b.clone();invalid[cod+4+offset]=value;assert!(coarse(&invalid,0,None,None).is_none());}
 }
}
