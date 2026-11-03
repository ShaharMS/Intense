//! PLT packet addresses continue across tile parts, not from packet zero.
//! Part boundaries need not coincide with resolution/component/layer boundaries.
use super::*;

struct IndexedPart {part:TilePart,header:Vec<u8>,start:u64,lengths:Arc<Vec<u64>>,packet_start:usize}

fn selected_spans(h:&Header,indexed:&IndexedPart,packets:&[Packet],reduction:u8,layers:u32,
    roi:Option<&precinct_roi::Selection>)->Result<Vec<(bool,Range<u64>)>> {
    let mut cursor=indexed.start;let mut spans=Vec::with_capacity(indexed.lengths.len());
    for (packet,&length) in packets[indexed.packet_start..].iter().zip(indexed.lengths.iter()) {
        ensure!(length>0,"invalid zero-length multipart PLT packet");
        let end=cursor.checked_add(length).context("multipart PLT packet length overflow")?;
        ensure!(end<=indexed.part.offset+indexed.part.length,"multipart PLT packet exceeds tile part");
        let keep=packet.resolution<=h.levels-1-reduction && (layers==0 || u32::from(packet.layer)<layers)
            && roi.is_none_or(|roi|roi.keeps(h,packet));
        spans.push((keep,cursor..end));cursor=end;
    }
    ensure!(cursor==indexed.part.offset+indexed.part.length,"multipart PLT lengths do not cover tile-part payload");
    Ok(spans)
}

fn retained_sot(part:&TilePart,length:usize)->Result<[u8;12]> {
    // Every SOT field was validated while parsing this immutable tile part.
    // Psot describes the temporary subset, so it is rewritten in either path.
    let mut sot=[0xff,0x90,0,10,0,0,0,0,0,0,part.ordinal,part.total];
    sot[4..6].copy_from_slice(&part.tile.to_be_bytes());
    sot[6..10].copy_from_slice(&u32::try_from(length)?.to_be_bytes());Ok(sot)
}

/// Validate the complete tile's ordinal chain before interpreting packet counts.
/// TNsot=0 is legal: the final packet count still has to match COD/SIZ geometry.
fn chain(parts:&[(u8,u8,usize)],expected:usize)->Result<Vec<usize>> {
    let mut starts=Vec::with_capacity(parts.len());let mut count=0usize;
    ensure!(parts.len()<=256,"multipart PLT tile exceeds ordinal capacity");
    for (ordinal,&(actual,total,packets)) in parts.iter().enumerate() {
        ensure!(usize::from(actual)==ordinal,"multipart PLT tile-part ordinals are missing or duplicated");
        ensure!(total==0 || usize::from(total)==parts.len(),"multipart PLT tile-part total disagrees with discovered chain");
        starts.push(count);count=count.checked_add(packets).context("multipart PLT packet count overflow")?;
        ensure!(count<=expected,"multipart PLT packet count exceeds codestream geometry");
    }
    ensure!(count==expected,"multipart PLT packet count does not cover codestream geometry");
    Ok(starts)
}

impl RemoteSource {
    pub(super) fn prepare_multipart(&self,parts:&[TilePart],payload:&HeaderPayload,reduction:u8,layers:u32,
        roi:Option<&precinct_roi::Selection>,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
        if !std::env::var("RIVER_S3_MULTIPART_PLT").is_ok_and(|v|v=="true" || v=="1") {return Ok(None);}
        self.prepare_indexed_multipart(parts,payload,reduction,layers,roi,context)
    }
    pub(super) fn prepare_indexed_multipart(&self,parts:&[TilePart],payload:&HeaderPayload,reduction:u8,layers:u32,
        roi:Option<&precinct_roi::Selection>,context:&mut ReadContext)->Result<Option<PreparedSubset>> {
        if !self.header.packet_compatible || parts.len()>4096
            || !parts.iter().any(|p|p.ordinal>0 || p.total>1) {return Ok(None);}
        #[cfg(all(feature="grok",unix))] if context.pipeline.is_some() {return Ok(None);}
        let h=&self.header;
        #[cfg(not(test))]let temporary_order=true;
        #[cfg(test)]let temporary_order=!context.legacy_multipart_plan;
        // Read only bounded headers before committing to the selective path.
        // A missing PLT/changed progression/packed header uses existing fallback.
        self.prefetch_tile_headers(parts,4,context)?;
        let mut groups=BTreeMap::<u16,Vec<IndexedPart>>::new();
        for part in parts {
            context.check()?;
            let (header,start,lengths,compatible)=self.tile_header(part,context)?;
            let Some(lengths)=lengths.filter(|_|compatible) else{return Ok(None);};
            groups.entry(part.tile).or_default().push(IndexedPart {part:part.clone(),header,start,lengths,packet_start:0});
        }
        // Preflight every chain before entropy I/O. A malformed index must give
        // a specific error, never silently select another part's packet bytes.
        for (&tile,group) in &mut groups {
            group.sort_unstable_by_key(|p|p.part.ordinal);
            let expected=if temporary_order {tile_packet_count(h,tile).context("packet count overflow")?}
                else {self.ordered_packets(tile,None,context)?.1 as u64};
            ensure!(expected<=MAX_PACKETS as u64,"packet index exceeds request memory budget");
            let descriptors=group.iter().map(|p|(p.part.ordinal,p.part.total,p.lengths.len())).collect::<Vec<_>>();
            for (part,start) in group.iter_mut().zip(chain(&descriptors,expected as usize)?) {part.packet_start=start;}
        }
        // Some decoders misrender coarsest PCRL multipart streams even when
        // decoding the complete original. Normalize only this temporary view:
        // one RPCL part per tile, original LL packet entropy, empty high bands.
        // This also exposes the existing packed OpenHT lowest-resolution path.
        if h.layers==1 && h.progression==3 && reduction+1==h.levels
            && groups.values().all(|parts|parts.iter().skip(1).all(|p|p.header.is_empty())) {
            return self.normalized_lowest(&groups,payload,context).map(Some);
        }
        let mut codestream=payload.main.clone();
        if !self.append_batched_parts(&groups,reduction,layers,roi,temporary_order,&mut codestream,context)? {
        for (&tile,group) in &groups {
            // Each tile's order is consumed once. Retaining it in the request
            // cache adds clones and keeps descriptors alive across later tiles.
            let (packets,_)=if temporary_order {packet_plan_cache::Cache::shared().get_or_build(h,tile,None)?.0}else{self.ordered_packets(tile,None,context)?};
            for indexed in group {
                context.check()?;
                let spans=selected_spans(h,indexed,&packets,reduction,layers,roi)?;
                self.append_indexed_part(indexed,&spans,&mut codestream,context)?;
            }
        }
        }
        codestream.extend([0xff,0xd9]);let jp2=!payload.container.is_empty();
        let bytes=if jp2 {
            let mut bytes=payload.container.clone();bytes.extend(u32::try_from(codestream.len()+8)?.to_be_bytes());bytes.extend(b"jp2c");bytes.extend(codestream);bytes
        }else{codestream};
        Ok(Some(PreparedSubset {view_reduction:0,technical_origin:None,bytes,jp2,route:"s3-multipart-packet-ranges",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0}))
    }

    fn append_batched_parts(&self,groups:&BTreeMap<u16,Vec<IndexedPart>>,reduction:u8,layers:u32,
        roi:Option<&precinct_roi::Selection>,temporary_order:bool,codestream:&mut Vec<u8>,context:&mut ReadContext)->Result<bool> {
        let count=groups.values().flatten().fold(0usize,|n,p|n.saturating_add(p.lengths.len()));
        if count>65536 {return Ok(false);}
        let parts=groups.values().map(Vec::len).sum::<usize>();
        let scratch=count*2*std::mem::size_of::<(bool,Range<u64>)>()+parts*64
            +(8*1024*1024/self.block_size as usize)*64;
        #[cfg(test)] if std::env::var_os("RIVER_TEST_MULTIPART_BATCH_DIAG").is_some() {
            eprintln!("MULTIPART_BATCH_DIAG count={count} scratch={scratch} used={} limit={}",context.metadata_bytes,context.metadata_limit);
        }
        if !context.admit_metadata(scratch) {return Ok(false);}
        let result=(|| ->Result<bool> {
            let mut plans=Vec::with_capacity(parts);let mut demand=Vec::with_capacity(count);
            for (&tile,group) in groups {
                context.check()?;
                let (packets,_)=if temporary_order {packet_plan_cache::Cache::shared().get_or_build(&self.header,tile,None)?.0}
                    else {self.ordered_packets(tile,None,context)?};
                for indexed in group {
                    let spans=selected_spans(&self.header,indexed,&packets,reduction,layers,roi)?;
                    demand.extend(spans.iter().filter(|(keep,_)|*keep).cloned());plans.push((indexed,spans));
                }
            }
            if !context.can_pin_ranges(self.block_size,&demand) {
                #[cfg(test)] if std::env::var_os("RIVER_TEST_MULTIPART_BATCH_DIAG").is_some() {eprintln!("MULTIPART_BATCH_DIAG decline=retention");}
                return Ok(false);
            }
            // Do not turn a legal per-part fallback into an aggregate quota
            // failure. This upper bound includes blocks already in the caches.
            let blocks=demand.iter().flat_map(|(_,r)|r.start/self.block_size..r.end.div_ceil(self.block_size))
                .map(|block|block*self.block_size).collect::<BTreeSet<_>>();
            let ranges=contiguous_block_ranges(blocks.into_iter(),self.block_size,self.meta.size,1024*1024).len();
            #[cfg(test)] if std::env::var_os("RIVER_TEST_MULTIPART_BATCH_DIAG").is_some() {eprintln!("MULTIPART_BATCH_DIAG ranges={ranges}");}
            if ranges>48 {return Ok(false);}
            let mut leases=self.prefetch_ranges(&demand,context,48)?;
            // Gap overfetch may be useful for transport but must not overflow
            // the8MiB retention bound and cause another fetch during assembly.
            retain_requested_blocks(&mut leases,&demand,self.block_size);
            context.pin_packet_blocks(leases);
            for (indexed,spans) in plans {self.append_indexed_part(indexed,&spans,codestream,context)?;}
            Ok(true)
        })();
        context.metadata_bytes=context.metadata_bytes.saturating_sub(scratch);result
    }

    fn append_indexed_part(&self,indexed:&IndexedPart,spans:&[(bool,Range<u64>)],codestream:&mut Vec<u8>,context:&mut ReadContext)->Result<()> {
        context.check()?;
        ensure!(spans.last().map_or(indexed.start,|(_,r)|r.end)==indexed.part.offset+indexed.part.length,
            "multipart PLT lengths do not cover tile-part payload");
        let selected=self.selected_payload(spans,context)?;
        let sot=retained_sot(&indexed.part,12+indexed.header.len()+2+selected.len())?;
        codestream.extend(sot);codestream.extend_from_slice(&indexed.header);codestream.extend([0xff,0x93]);codestream.extend(selected);
        ensure!(codestream.len()<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"compressed subset exceeds request memory budget");
        Ok(())
    }

    fn normalized_lowest(&self,groups:&BTreeMap<u16,Vec<IndexedPart>>,payload:&HeaderPayload,context:&mut ReadContext)->Result<PreparedSubset> {
        let h=&self.header;let mut stream=payload.main.clone();normalize_progression(&mut stream)?;
        for (&tile,parts) in groups {
            let (packets,total)=self.ordered_packets(tile,None,context)?;let mut selected=vec![];
            for part in parts {
                let mut cursor=part.start;
                for (packet,&length) in packets[part.packet_start..].iter().zip(part.lengths.iter()) {
                    ensure!(length>0,"invalid zero-length multipart PLT packet");
                    let end=cursor.checked_add(length).context("multipart PLT packet length overflow")?;
                    ensure!(end<=part.part.offset+part.part.length,"multipart PLT packet exceeds tile part");
                    if packet.resolution==0 {selected.push((packet.clone(),cursor..end));}
                    cursor=end;
                }
                ensure!(cursor==part.part.offset+part.part.length,"multipart PLT lengths do not cover tile-part payload");
            }
            selected.sort_unstable_by_key(|(p,_)|(p.y,p.x,p.component,p.layer));
            let spans=selected.iter().map(|(_,range)|(true,range.clone())).collect::<Vec<_>>();
            let mut bytes=self.selected_payload(&spans,context)?;
            if h.cod[0]&2!=0 {
                let mut at=0;
                for (ordinal,(_,range)) in selected.iter().enumerate() {
                    ensure!(bytes.get(at..at+4)==Some(&[0xff,0x91,0,4]),"missing SOP in normalized lowest-resolution packet");
                    bytes[at+4..at+6].copy_from_slice(&(ordinal as u16).to_be_bytes());at+=usize::try_from(range.end-range.start)?;
                }
            }
            for ordinal in selected.len()..total {append_empty_packet(&mut bytes,h.cod[0],ordinal);}
            let first=&parts[0];let mut sot=retained_sot(&first.part,12+first.header.len()+2+bytes.len())?;
            sot[10]=0;sot[11]=1;
            stream.extend(sot);stream.extend_from_slice(&first.header);stream.extend([0xff,0x93]);stream.extend(bytes);
            ensure!(stream.len()<=context.limit as usize+MAX_HEADER+MAX_PACKETS,"compressed subset exceeds request memory budget");
        }
        stream.extend([0xff,0xd9]);let jp2=!payload.container.is_empty();
        let bytes=if jp2 {let mut bytes=payload.container.clone();bytes.extend(u32::try_from(stream.len()+8)?.to_be_bytes());bytes.extend(b"jp2c");bytes.extend(stream);bytes}else{stream};
        Ok(PreparedSubset {view_reduction:0,technical_origin:None,bytes,jp2,route:"s3-multipart-lowest-rpcl-ranges",width:h.width,height:h.height,layout:h.layout,offset_x:0,offset_y:0})
    }
}

fn normalize_progression(main:&mut [u8])->Result<()> {
    let mut at=2;
    while at+4<=main.len() {
        let marker=be16(main,at)?;let length=usize::from(be16(main,at+2)?);
        ensure!(length>=2 && at+2+length<=main.len(),"invalid normalized main header");
        if marker==0xff52 {ensure!(length>=12,"truncated normalized COD");main[at+5]=2;return Ok(());}
        at+=length+2;
    }
    bail!("normalized COD missing")
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn packet_position_continues_across_arbitrary_part_boundaries() {
        assert_eq!(chain(&[(0,3,79),(1,3,401),(2,3,721)],1201).unwrap(),vec![0,79,480]);
        assert_eq!(chain(&[(0,0,79),(1,0,401),(2,0,721)],1201).unwrap(),vec![0,79,480]);
    }
    #[test] fn malformed_chains_have_specific_errors() {
        for (parts,expected,message) in [
            (vec![(0,2,3),(0,2,4)],7,"ordinals"),
            (vec![(0,3,3),(1,3,4)],7,"total"),
            (vec![(0,2,3),(1,2,4)],6,"exceeds"),
            (vec![(0,2,3),(1,2,4)],8,"does not cover"),
        ] {assert!(chain(&parts,expected).unwrap_err().to_string().contains(message));}
    }
}
