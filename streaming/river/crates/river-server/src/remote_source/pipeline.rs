//! Experimental bounded decode overlap using version-validated native tiles.
use super::*;
use std::sync::{atomic::AtomicBool,mpsc};
static BUSY:AtomicBool=AtomicBool::new(false);
struct Admission;
impl Drop for Admission {fn drop(&mut self){BUSY.store(false,Ordering::Release);}}
type Message=(Vec<u16>,crate::decoder::grok::PipelineView);
pub(super) struct Pipeline {
    seen:usize,full:bool,known:Vec<Option<u64>>,target:HashSet<u16>,expected:usize,origin:(u32,u32),ids:Vec<bool>,pending:Vec<TilePart>,batch:Vec<(u16,Vec<u8>)>,sent:usize,reduction:u8,batch_limit:usize,
    sender:Option<mpsc::SyncSender<Message>>,
    worker:Option<std::thread::JoinHandle<Result<(image::RgbaImage,f64)>>>,
}
impl Drop for Pipeline {fn drop(&mut self){self.sender.take();if let Some(worker)=self.worker.take(){let _=worker.join();}}}
impl Pipeline {
    pub(super) fn start(source:&RemoteSource,reduction:u8,layers:u32,selected:&HashSet<u16>,cancel:Option<watch::Receiver<bool>>)->Option<Self> {
        let h=&source.header;let scale=1u32.checked_shl(u32::from(reduction))?;
        let native=u64::from(h.layout.columns)*u64::from(h.layout.rows);
        let precinct=*h.cod.get(10)?;let samples=(1u32<<(precinct&15)).min(h.layout.width/scale).saturating_mul((1u32<<(precinct>>4)).min(h.layout.height/scale));
        if std::env::var("RIVER_OPENHT_PIPELINE").is_ok_and(|v|v=="false" || v=="0")
            || std::env::var("RIVER_OPENHT_DIRECT_BLOCKS").is_ok_and(|v|v=="false" || v=="0")
            || std::env::var("RIVER_OPENHT_MIN_PACKETS").is_ok_and(|v|!v.is_empty() && v.parse::<usize>().is_ok()) || selected.len()<16 || reduction==0
            || reduction+1!=h.levels || h.is_ht || !h.prefix_compatible || h.layers!=1 || layers>1
            || h.image_origin!=(0,0) || !h.layout.width.is_power_of_two() || !h.layout.height.is_power_of_two()
            || h.width%h.layout.width!=0 || h.height%h.layout.height!=0 || native<1024 || native>16384
            || samples<64 || source.meta.size<64*1024*1024 || source.cache.limit<8*1024*1024
            || crate::source_admission::active_preparations()>1 {return None;}
        let full=selected.len()==native as usize;
        let (x0,y0,x1,y1)=if full {(0,0,h.layout.columns,h.layout.rows)}else{
            (selected.iter().map(|id|u32::from(*id)%h.layout.columns).min()?,
             selected.iter().map(|id|u32::from(*id)/h.layout.columns).min()?,
             selected.iter().map(|id|u32::from(*id)%h.layout.columns).max()?+1,
             selected.iter().map(|id|u32::from(*id)/h.layout.columns).max()?+1)
        };
        let tw=h.layout.width/scale;let th=h.layout.height/scale;
        let width=(x1-x0)*tw;let height=(y1-y0)*th;
        if u64::from(width)*u64::from(height)>4*1024*1024 {return None;}
        if BUSY.compare_exchange(false,true,Ordering::Acquire,Ordering::Relaxed).is_err(){return None;}
        let admission=Admission;let columns=h.layout.columns;let expected=selected.len();
        let (sender,receiver)=mpsc::sync_channel::<Message>(2);
        let worker=std::thread::Builder::new().name("openht-prefix-pipeline".into()).spawn(move||{
            let _admission=admission;let _cancel=crate::decoder::grok::cancellation_scope(cancel);
            let mut image=image::RgbaImage::new(width,height);let mut covered=0usize;let mut work_ms=0.0;
            while let Ok((ids,view))=receiver.recv(){
                let began=Instant::now();let decoded=crate::decoder::grok::decode_pipeline_view(view)?;work_ms+=crate::decoder::elapsed_ms(began);
                ensure!(decoded.height()==th && decoded.width()>=ids.len() as u32*tw,"pipeline batch geometry mismatch");
                let input=decoded.as_raw();let output=image.as_mut();
                for (i,id) in ids.iter().enumerate(){
                    let x=(u32::from(*id)%columns).checked_sub(x0).context("pipeline tile precedes selected columns")?*tw;
                    let y=(u32::from(*id)/columns).checked_sub(y0).context("pipeline tile precedes selected rows")?*th;
                    ensure!(x+tw<=width && y+th<=height,"pipeline tile exceeds selected raster");
                    for row in 0..th as usize {
                        let from=(row*decoded.width() as usize+i*tw as usize)*4;
                        let to=((y as usize+row)*width as usize+x as usize)*4;
                        output[to..to+tw as usize*4].copy_from_slice(&input[from..from+tw as usize*4]);
                    }
                }
                covered+=ids.len();
            }
            ensure!(covered==expected,"pipeline image has incomplete tile coverage");Ok((image,work_ms))
        }).ok()?;
        Some(Self{seen:0,full,known:if full {vec![]}else{vec![None;native as usize]},target:if full {HashSet::new()}else{selected.clone()},expected,origin:(x0*tw,y0*th),ids:vec![false;native as usize],pending:vec![],batch:vec![],sent:0,reduction,batch_limit:env_limit("RIVER_OPENHT_PIPELINE_BATCH_TILES",256).clamp(32,512),sender:Some(sender),worker:Some(worker)})
    }
    fn flush(&mut self,source:&RemoteSource,context:&mut ReadContext)->Result<bool>{
        if self.batch.is_empty(){return Ok(true);}
        let Some(view)=source.pipeline_view(&self.batch,self.reduction,context)? else{return Ok(false);};
        let ids=self.batch.iter().map(|(id,_)|*id).collect::<Vec<_>>();let count=ids.len();let mut message=(ids,view);
        loop {context.check()?;match self.sender.as_ref().unwrap().try_send(message){
            Ok(())=>break,
            Err(mpsc::TrySendError::Full(returned))=>{message=returned;std::thread::sleep(Duration::from_millis(1));},
            Err(mpsc::TrySendError::Disconnected(_))=>{
                context.check()?;
                if std::env::var("RIVER_OPENHT_STRICT").as_deref()!=Ok("true") {
                    tracing::warn!("OpenHT overlap worker closed its input; continuing source preparation");
                    return Ok(false);
                }
                if let Some(worker)=self.worker.take(){match worker.join(){Ok(Err(error))=>return Err(error).context("pipeline decoder failed"),Err(_)=>bail!("pipeline decoder thread stopped unexpectedly"),_=>{}}}
                bail!("pipeline decoder closed its input before completing the image");
            },
        }}
        self.sent+=count;self.batch.clear();Ok(true)
    }
    fn collect(&mut self,source:&RemoteSource,part:TilePart,context:&mut ReadContext)->Result<bool>{
        if part.ordinal!=0 || part.total!=1 || self.ids.get(usize::from(part.tile)).is_none_or(|seen|*seen){return Ok(false);}
        let previous=context.cache_only;context.cache_only=true;let before=context.stats.clone();
        // Lease a few already fetched prefix blocks while parsing this tile.
        // Packet-level reads then avoid repeatedly touching the shared LRU.
        let old=std::mem::take(&mut context.pinned);
        for block in part.offset/source.block_size..part.offset.saturating_add(4096).min(part.offset+part.length).div_ceil(source.block_size) {
            let offset=block*source.block_size;
            if let Some(bytes)=old.get(&offset).cloned().or_else(||context.recent.get(source.block_size,offset)).or_else(||source.cache.get(&source.cache_key,source.block_size,offset)) {context.pinned.insert(offset,bytes);}
        }
        let result=source.cached_lowest_tile(&part,self.reduction,context);context.pinned=old;context.cache_only=previous;
        let bytes=match result{
            Ok(Some(bytes))=>bytes,Ok(None)=>return Ok(false),
            Err(error) if error.downcast_ref::<PrefixNotCached>().is_some()=>{self.pending.push(part);return Ok(true);},
            Err(error)=>return Err(error).context("pipeline packet preparation failed"),
        };
        ensure!(context.stats.bytes==before.bytes && context.stats.requests==before.requests,"cached pipeline issued source IO");
        self.ids[usize::from(part.tile)]=true;self.batch.push((part.tile,bytes));
        if self.batch.len()>=self.batch_limit {return self.flush(source,context);}Ok(true)
    }
    fn ready(&mut self,source:&RemoteSource,parts:&[TilePart],context:&mut ReadContext,finalize:Option<(u64,u64)>)->Result<bool>{
        let incoming=if finalize.is_some() && !self.full {parts}else{parts.get(self.seen..).context("pipeline index prefix shrank")?};
        for part in incoming {
            if !self.full {
            if !self.target.contains(&part.tile){continue;}
            let Some(known)=self.known.get_mut(usize::from(part.tile)) else{return Ok(false);};
            if let Some(offset)=known {if *offset==part.offset {continue;}return Ok(false);}
            *known=Some(part.offset);
            }
            if !self.collect(source,part.clone(),context)? {return Ok(false);}
        }
        if finalize.is_none() || self.full {self.seen=parts.len();}
        if let Some((payload_base,index_credit))=finalize {
            context.allow_prefix_ranges(parts.len() as u64,payload_base,index_credit);
            // Only unresolved, selected LL prefixes enter normal bounded IO.
            // Completed tiles are already owned by the decoder queue/output.
            let mut prefixes=vec![];
            for part in &self.pending {let (_,start,_,_)=source.tile_header(part,context)?;prefixes.push((part.clone(),start));}
            source.prefetch_lowest_prefixes(&prefixes,context)?;
            let pending=std::mem::take(&mut self.pending);
            for part in pending {if !self.collect(source,part,context)? {return Ok(false);}}
            if !self.flush(source,context)? {return Ok(false);}
        }
        Ok(true)
    }
    pub(super) fn complete(&self)->bool {self.sent==self.expected}
    pub(super) fn finish(mut self)->Result<Option<(Arc<image::RgbaImage>,f64,(u32,u32))>>{
        if !self.complete(){return Ok(None);}
        self.sender.take();let result=self.worker.take().unwrap().join().map_err(|_|anyhow::anyhow!("pipeline decoder thread stopped unexpectedly"))??;
        tracing::info!(decode_work_ms=result.1,tiles=self.sent,"OpenHT pipeline complete");
        Ok(Some((Arc::new(result.0),result.1,self.origin)))
    }
}
#[derive(Debug)]pub(super) struct PrefixNotCached;
impl std::fmt::Display for PrefixNotCached {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"pipeline prefix is not cached")}}
impl std::error::Error for PrefixNotCached {}
impl RemoteSource {
    pub(super) fn resolve_pipeline(&self,context:&mut ReadContext)->Result<bool> {
        let Some(pipeline)=context.pipeline.take() else {return Ok(false);};
        let result=pipeline.finish();
        context.check()?;
        match result {
            Ok(output)=>{
                context.pipeline_output=output;
                Ok(context.pipeline_output.is_some())
            }
            Err(error) if std::env::var("RIVER_OPENHT_STRICT").as_deref()!=Ok("true")=>{
                tracing::warn!(%error,"OpenHT overlap failed; continuing source preparation");
                Ok(false)
            }
            Err(error)=>Err(error),
        }
    }
    fn cached_lowest_tile(&self,part:&TilePart,reduction:u8,context:&mut ReadContext)->Result<Option<Vec<u8>>>{
        let (header,start,lengths,compatible)=self.tile_header(part,context)?;
        if !header.is_empty() || lengths.is_some() || !compatible {return Ok(None);}
        let (packets,_)=self.ordered_packets(part.tile,Some(0),context)?;
        self.prefix_payload(part,start,&packets,packets.len(),reduction,None,context)
    }
    // Private LL packet envelope consumed only by the conservative packer.
    // Padding repeats at most15 complete native tiles; those pixels are ignored.
    fn pipeline_view(&self,batch:&[(u16,Vec<u8>)],reduction:u8,context:&mut ReadContext)->Result<Option<crate::decoder::grok::PipelineView>> {
        let h=&self.header;let payload=self.acquire_header(context)?;let count=batch.len().max(16);
        let width=u32::try_from(count)?.checked_mul(h.layout.width).context("pipeline width overflow")?;let height=h.layout.height;
        let mut stream=payload.main.clone();resize_siz(&mut stream,width,height)?;
        for n in 0..count {
            let bytes=&batch[n.min(batch.len()-1)].1;
            stream.extend([255,144,0,10]);stream.extend(u16::try_from(n)?.to_be_bytes());
            stream.extend(u32::try_from(14+bytes.len())?.to_be_bytes());stream.extend([0,1,255,147]);stream.extend(bytes);
            ensure!(stream.len()<=context.limit as usize,"pipeline batch exceeds request memory budget");
        }
        stream.extend([255,217]);
        let bytes=if payload.container.is_empty(){stream}else{
            let mut container=payload.container.clone();resize_jp2(&mut container,width,height)?;
            container.extend(u32::try_from(stream.len()+8)?.to_be_bytes());container.extend(b"jp2c");container.extend(stream);container
        };
        Ok(crate::decoder::grok::pipeline_view(&bytes,reduction))
    }
    pub(crate) fn start_pipeline(&self,context:&mut ReadContext){context.pipeline_requested=true;}
    pub(super) fn pipeline_final(&self,parts:&[TilePart],context:&mut ReadContext,payload_base:u64,index_credit:u64)->Result<()> {
        if let Some(mut pipeline)=context.pipeline.take(){if pipeline.ready(self,parts,context,Some((payload_base,index_credit)))? {context.pipeline=Some(pipeline);}}Ok(())
    }
    pub(super) fn pipeline_ready(&self,parts:&[TilePart],context:&mut ReadContext)->Result<()> {
        if let Some(mut pipeline)=context.pipeline.take(){if pipeline.ready(self,parts,context,None)? {context.pipeline=Some(pipeline);}}Ok(())
    }
}
