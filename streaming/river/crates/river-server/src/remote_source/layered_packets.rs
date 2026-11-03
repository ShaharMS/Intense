//! Stateful Part-1 packet lengths for resolution-first multilayer prefixes.
//! No entropy decoding: preserve inclusion/zero-plane trees and Lblock across
//! layers of one precinct. Classic Part1 segment boundaries persist across layers.
use super::*;

#[derive(Clone)]
struct Block {passes:u64,lblock:u32}
#[derive(Clone)]
struct Band {inclusion:TagTree,zero_planes:TagTree,blocks:Vec<Block>}
#[derive(Clone)]
pub(super) struct State {bands:Vec<Band>,pending_grids:Option<Vec<(usize,usize)>>,next_layer:u16,style:u8}
fn segment_remaining(style:u8,passes:u64)->u64 {
 if style&4!=0 {1}else if style&1!=0 {
  if passes<10 {10-passes}else{let phase=(passes-10)%3;if phase<2 {2-phase}else{1}}
 }else{109-passes}
}
impl State {
 #[cfg(test)] fn new(grids:&[(usize,usize)],style:u8)->Self {
  let mut state=Self::new_lazy(grids,style);state.initialize();state
 }
 pub(super) fn new_lazy(grids:&[(usize,usize)],style:u8)->Self {
  Self {next_layer:0,style,bands:vec![],pending_grids:Some(grids.to_vec())}
 }
 fn initialize(&mut self) {
  if let Some(grids)=self.pending_grids.take() {
   self.bands=grids.into_iter().map(|(w,h)|Band {inclusion:TagTree::new(w,h),zero_planes:TagTree::new(w,h),blocks:vec![Block {passes:0,lblock:3};w*h]}).collect();
  }
 }
 pub(super) fn length(&mut self,bytes:&[u8],layer:u16)->Result<(usize,u64)> {
  // An empty packet cannot change tag trees, coding passes or Lblock. Its
  // presence bit therefore needs no transactional heap/state clone.
  if bytes.first().is_some_and(|byte|byte&0x80==0) {
   ensure!(layer==self.next_layer,"multilayer packet order is not consecutive");
   self.next_layer=self.next_layer.checked_add(1).context("multilayer layer overflow")?;
   return Ok((1,0));
  }
  // A short probe must not consume persistent tag-tree/codeblock state.
  let mut trial=self.clone();let result=trial.parse(bytes,layer)?;*self=trial;Ok(result)
 }
 // Only for a complete immutable tile-part buffer. Any error must abort the
 // preparation; unlike length(), this method deliberately permits mutation
 // on failure and therefore must never be used for growing/retried probes.
 pub(super) fn length_from_complete_part(&mut self,bytes:&[u8],layer:u16)->Result<(usize,u64)> {
  if bytes.first().is_some_and(|byte|byte&0x80==0) {self.length(bytes,layer)}else{self.parse(bytes,layer)}
 }
 fn parse(&mut self,bytes:&[u8],layer:u16)->Result<(usize,u64)> {
  ensure!(layer==self.next_layer,"multilayer packet order is not consecutive");
  self.initialize();
  let mut bits=Bits {bytes,offset:0,remaining:0,byte:0,previous:0};let mut length=0u64;
  if bits.bit()?!=0 {
   for band in &mut self.bands {for (index,block) in band.blocks.iter_mut().enumerate() {
    let first=block.passes==0;
    let included=if first {band.inclusion.decode(index,u32::from(layer)+1,&mut bits)?}else{bits.bit()?!=0};
    if !included {continue;}
    if first {ensure!(band.zero_planes.decode(index,32,&mut bits)?,"invalid multilayer zero-bitplane tree");}
    let passes=if bits.bit()?==0 {1}else if bits.bit()?==0 {2}else {
     let n=bits.bits(2)?;if n<3 {3+n}else{let n=bits.bits(5)?;if n<31 {6+n}else{37+bits.bits(7)?}}
    };
    ensure!(block.passes+passes<=109,"multilayer codeblock exceeds supported segment passes");
    while bits.bit()?!=0 {block.lblock+=1;ensure!(block.lblock<=31,"invalid multilayer Lblock");}
    let mut remaining=passes;
    while remaining>0 {
     let segment=remaining.min(segment_remaining(self.style,block.passes));
     ensure!(segment>0,"invalid multilayer segment boundary");
     let count=block.lblock+63-segment.leading_zeros();
     ensure!(count<=32,"invalid multilayer segment length bits");
     length=length.checked_add(bits.bits(count)?).context("multilayer body length overflow")?;
     block.passes+=segment;remaining-=segment;
    }
   }}
  }
  if bits.previous==255 {bytes.get(bits.offset).ok_or(ProbeTooShort)?;bits.offset+=1;}
  self.next_layer=self.next_layer.checked_add(1).context("multilayer layer overflow")?;
  Ok((bits.offset,length))
 }
}

#[cfg(test)] mod tests {
 use super::*;
 #[test] fn complete_buffers_match_transactional_lengths_for_all_classical_styles() {
  for style in 0..64 {
   let mut transactional=State::new_lazy(&[(1,1)],style);let mut complete=transactional.clone();
   for (layer,data) in [vec![0xe3],vec![0xc8],vec![0],vec![0xc8]].into_iter().enumerate() {
    assert_eq!(complete.length_from_complete_part(&data,layer as u16).unwrap(),transactional.length(&data,layer as u16).unwrap());
   }
   assert!(complete.length_from_complete_part(&[0xff],4).is_err());
   assert!(transactional.length(&[0xff],4).is_err());assert_eq!(transactional.next_layer,4);
  }
 }
 #[test] fn lazy_empty_layers_allocate_no_band_state_and_failed_probes_remain_lazy() {
  for style in 0..64 {
  let mut state=State::new_lazy(&[(1,1)],style);
  assert_eq!(state.length(&[0],0).unwrap(),(1,0));
  assert!(state.bands.is_empty() && state.pending_grids.is_some());
  assert!(state.length(&[0xff],1).is_err());
  assert_eq!(state.next_layer,1);assert!(state.bands.is_empty() && state.pending_grids.is_some());
  let mut eager=State::new(&[(1,1)],style);eager.length(&[0],0).unwrap();
  // First inclusion in layer1: skip layer0 in the inclusion tree.
  let data=bytes("101100011");
  assert_eq!(state.length(&data,1).unwrap(),eager.length(&data,1).unwrap());
  assert_eq!(state.bands[0].blocks[0].passes,1);assert!(state.pending_grids.is_none());
  assert_eq!(state.length(&[0],2).unwrap(),(1,0));assert_eq!(state.next_layer,3);
  }
 }
 fn bytes(bits:&str)->Vec<u8> {
  let mut out=vec![0;bits.len().div_ceil(8)];
  for (n,b) in bits.bytes().enumerate() {if b==b'1' {out[n/8]|=1<<(7-n%8);}}
  out
 }
 #[test] fn terminated_passes_have_separate_lengths() {
  let mut state=State::new(&[(1,1)],4);
  let data=bytes("111100010011");
  assert_eq!(state.length(&data,0).unwrap(),(2,5));
  assert_eq!(state.bands[0].blocks[0].passes,2);
  assert_eq!(state.length(&[0xc8],1).unwrap(),(1,4));
 }
 #[test] fn bypass_continues_a_partly_filled_segment_from_an_earlier_layer() {
  let mut state=State::new(&[(1,1)],1);state.next_layer=1;state.bands[0].blocks[0].passes=9;
  // Four passes cross the last initial MQ pass, two raw passes, and cleanup.
  let data=bytes("11110100100011100");
  assert_eq!(state.length(&data,1).unwrap(),(3,9));
  assert_eq!(state.bands[0].blocks[0].passes,13);
  assert_eq!((10..16).map(|p|segment_remaining(1,p)).collect::<Vec<_>>(),vec![2,1,1,2,1,1]);
 }
 #[test] fn empty_layers_advance_and_short_probes_do_not_mutate_state() {
  let mut state=State::new(&[(1,1)],0);
  assert!(state.length(&[],0).is_err());assert_eq!(state.next_layer,0);
  assert_eq!(state.length(&[0],0).unwrap(),(1,0));
  assert!(state.length(&[],1).is_err());assert_eq!(state.next_layer,1);
  assert_eq!(state.length(&[0],1).unwrap(),(1,0));
  assert!(state.length(&[0],1).unwrap_err().to_string().contains("consecutive"));
 }
 #[test] fn included_codeblocks_keep_passes_and_lblock_across_layers_and_failed_probes() {
  let mut state=State::new(&[(1,1)],0);
  assert!(state.length(&[0xff],0).is_err());
  assert_eq!(state.next_layer,0);assert_eq!(state.bands[0].blocks[0].passes,0);
  // Present, first inclusion, zero planes, one pass, Lblock=3, three bytes.
  assert_eq!(state.length(&[0xe3],0).unwrap(),(1,3));
  assert!(state.length(&[0xff],1).is_err());
  assert_eq!(state.next_layer,1);assert_eq!(state.bands[0].blocks[0].lblock,3);
  // Already included: one new pass, grow Lblock to4, nine body bytes.
  assert_eq!(state.length(&[0xd4,0x80],1).unwrap(),(2,9));
  assert_eq!(state.bands[0].blocks[0].passes,2);
  assert_eq!(state.bands[0].blocks[0].lblock,4);
 }
 #[test] fn empty_layer_after_inclusion_preserves_every_codeblock_and_tree_state() {
  let mut state=State::new(&[(1,1)],0);assert_eq!(state.length(&[0xe3],0).unwrap(),(1,3));
  let tree=(state.bands[0].inclusion.single.value,state.bands[0].inclusion.single.low,state.bands[0].zero_planes.single.value,state.bands[0].zero_planes.single.low);
  assert_eq!(state.length(&[0x7f],1).unwrap(),(1,0));
  assert_eq!((state.bands[0].blocks[0].passes,state.bands[0].blocks[0].lblock),(1,3));
  assert_eq!((state.bands[0].inclusion.single.value,state.bands[0].inclusion.single.low,state.bands[0].zero_planes.single.value,state.bands[0].zero_planes.single.low),tree);
  assert_eq!(state.length(&[0xd4,0x80],2).unwrap(),(2,9));
 }
}
