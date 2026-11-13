//! On-demand diagnostics from already parsed metadata. Never fetch source bytes.
use super::*;
use serde_json::{Value,json};
impl Header {
 pub(crate) fn structure_summary(&self)->Value {
  let style=self.cod.get(8).copied();let transform=self.cod.get(9).copied();
  let checks=[("compatibleMainHeader",self.packet_compatible),("multipleQualityLayers",self.layers>1),
   ("classicPart1Coding",!self.is_ht),("noSopEphOrOtherScodBits",self.cod.first().is_some_and(|v|v&!1==0)),
   ("supportedClassicCodeblockStyle",style.is_some_and(|v|v&0xc0==0)),("recognizedWavelet",transform.is_some_and(|v|v<=1)),
   ("resolutionFirstProgression",matches!(self.progression,1|2))];
  let failed=checks.iter().filter(|(_,ok)|!*ok).map(|(name,_)|*name).collect::<Vec<_>>();
  let packet_geometry=if self.packet_compatible && self.levels>0 {
   multipart_prefix::counts(self,0,0).ok().map(|(coarse,total)|json!({"firstNativeTilePackets":total,"firstNativeTileCoarsestPackets":coarse,"ordinaryPacketArrayLimit":MAX_PACKETS}))
  }else{None};
  let enabled=|name:&str|std::env::var(name).is_ok_and(|v|v=="true" || v=="1");
  json!({"schemaVersion":1,"inspectionDoesNotFetchSource":true,"scope":"Already parsed main header. Tile headers and packet lengths are not inspected; candidate eligibility is not a guarantee of successful selective serving.",
   "width":self.width,"height":self.height,"imageOrigin":{"x":self.image_origin.0,"y":self.image_origin.1},
   "components":self.components,"displayComponents":if self.display_rgb_only{3}else{self.components},"extraBandsAreAlpha":!self.display_rgb_only,"maximumComponentPrecisionBits":self.bit_depth,
   "nativeTiles":{"width":self.layout.width,"height":self.layout.height,"columns":self.layout.columns,"rows":self.layout.rows,
    "count":u64::from(self.layout.columns)*u64::from(self.layout.rows)},
   "resolutionLevels":self.levels,"qualityLayers":self.layers,
   "progression":{"code":self.progression,"name":(["LRCP","RLCP","RPCL","PCRL","CPRL"].get(usize::from(self.progression)))},
   "coding":{"scod":self.cod.first(),"mct":self.cod.get(4),"codeblockStyle":style,
    "codeblockWidth":self.cod.get(6).and_then(|v|1u32.checked_shl(u32::from(*v)+2)),
    "codeblockHeight":self.cod.get(7).and_then(|v|1u32.checked_shl(u32::from(*v)+2)),
    "waveletTransform":transform,"waveletName":match transform {Some(0)=>"irreversible9/7",Some(1)=>"reversible5/3",_=>"unrecognized"},
    "sop":self.cod.first().map(|v|v&2!=0),"eph":self.cod.first().map(|v|v&4!=0),
    "explicitPrecincts":self.cod.first().map(|v|v&1!=0),"codFixedFields":self.cod.iter().take(10).copied().collect::<Vec<_>>()},
   "selectiveChecks":{"packetCompatibleMainHeader":self.packet_compatible,"inlinePrefixCompatible":self.prefix_compatible,
    "multilayerInlineCandidate":multipart_prefix::inline_candidate(self),"failedMultilayerInlineChecks":failed},
   "packetGeometry":packet_geometry,"configuredReadFlags":{"singlePrefix":enabled("RIVER_S3_SINGLE_PREFIX"),"multipartPrefix":enabled("RIVER_S3_MULTIPART_PREFIX"),"multipartPlt":enabled("RIVER_S3_MULTIPART_PLT"),"precinctRoi":enabled("RIVER_S3_PRECINCT_ROI")},
   "firstTilePartOffset":self.first_part,"codestreamEndOffset":self.codestream_end,
   "tilePartIndex":{"state":"notIncluded"},"packetLengthIndexes":"notDeterminedFromMainHeader"})
 }
}
impl RemoteSource {
 pub(crate) fn structure_summary(&self)->Value {
  let mut result=self.header.structure_summary();result["sourceBytes"]=json!(self.meta.size);
  let payload=self.header_payload.lock().unwrap_or_else(|e|e.into_inner()).upgrade()
   .or_else(||HeaderCache::shared().get(&self.id));
  result["tilePartIndex"]=if let Some(payload)=payload {
   let mut per_tile=BTreeMap::<u16,usize>::new();let mut anonymous=0usize;
   for (tile,_) in &payload.tlm {if let Some(tile)=tile {*per_tile.entry(*tile).or_default()+=1;}else{anonymous+=1;}}
   let bytes=payload.tlm.iter().map(|(_,n)|u64::from(*n)).sum::<u64>();
   json!({"state":"resident","usableTlmEntries":payload.tlm.len(),"anonymousEntries":anonymous,
    "indexedNativeTiles":per_tile.len(),"minimumPartsPerIndexedTile":per_tile.values().min(),"maximumPartsPerIndexedTile":per_tile.values().max(),
    "declaredTilePartBytes":bytes,"coversCodestreamPayload":!payload.tlm.is_empty() && self.header.first_part.checked_add(bytes)==Some(self.header.codestream_end.saturating_sub(2)),
    "meaning":"TLM locates tile-parts, not packets. Zero usable entries does not prove that no raw TLM marker existed."})
  }else{json!({"state":"metadataEvicted","meaning":"Index summary unavailable without rereading. No source reads were performed; do not interpret this as an absent TLM."})};
  result
 }
}
#[cfg(test)] mod tests {
 use super::*;
 fn header()->Header {Header {image_origin:(0,0),width:2048,height:2048,components:3,display_rgb_only:false,bit_depth:12,levels:7,layers:10,
  layout:NativeTileLayoutHint {origin_x:0,origin_y:0,width:128,height:128,columns:16,rows:16},progression:2,is_ht:false,
  cod:vec![1,2,0,10,1,6,4,2,4,1,0x11,0x22,0x33,0x44,0x55,0x66,0x77],first_part:120000,codestream_end:1000000,
  packet_compatible:true,prefix_compatible:false,payload_digest:[0;32]}}
 #[test] fn cod_offsets_distinguish_codeblock_style_from_wavelet_transform() {
  let h=header();let s=h.structure_summary();
  assert_eq!(s["coding"]["codeblockWidth"],64);assert_eq!(s["coding"]["codeblockHeight"],16);
  assert_eq!(s["coding"]["codeblockStyle"],4);assert_eq!(s["coding"]["waveletTransform"],1);
  assert_eq!(s["nativeTiles"]["count"],256);assert_eq!(s["qualityLayers"],10);
  assert_eq!(s["selectiveChecks"]["failedMultilayerInlineChecks"],json!([]));
  assert!(multipart_prefix::inline_candidate(&h));
 }
 #[test] fn selective_diagnostic_tracks_the_actual_main_header_gate() {
  let mut h=header();h.cod[8]=0;
  for style in [0,1,4,64] {for flags in [0,1,2,4] {for transform in [0,1,4] {
   h.cod[8]=style;h.cod[0]=flags;h.cod[9]=transform;
   let s=h.structure_summary();
   assert_eq!(s["selectiveChecks"]["failedMultilayerInlineChecks"].as_array().unwrap().is_empty(),multipart_prefix::inline_candidate(&h));
   if transform==4 {assert_eq!(s["coding"]["waveletName"],"unrecognized");}
  }}}
 }
}
