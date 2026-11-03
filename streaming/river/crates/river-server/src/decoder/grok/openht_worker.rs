//! Experimental packed-LL backend. A process owns one persistent decoder;
//! failed/cancelled sessions are destroyed instead of returning partial state.
use anyhow::{Context,Result,bail,ensure};
use std::{collections::VecDeque,io::{Read,Write},os::{fd::OwnedFd,unix::net::UnixStream},process::{Child,Command,Stdio},sync::{Arc,Condvar,Mutex,OnceLock,mpsc,atomic::{AtomicUsize,Ordering}},thread::JoinHandle,time::{Duration,Instant}};
const REQUEST:u32=0x31544852;
const RESPONSE:u32=0x314f4852;
const MAX_INPUT:usize=66*1024*1024;
const MAX_PIXELS:u64=4*1024*1024;
const POLL:Duration=Duration::from_millis(50);
#[derive(Debug)] pub(super) struct Failure(pub anyhow::Error);
impl std::fmt::Display for Failure {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"OpenHT packed worker: {:#}",self.0)}}
impl std::error::Error for Failure {}
pub(super) fn available()->bool {
 let path=std::env::var_os("RIVER_OPENHT_WORKER").unwrap_or_else(||"/usr/local/bin/river-openht-worker".into());
 Command::new(path).arg("--self-test").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|v|v.success())
}
static DEMAND:AtomicUsize=AtomicUsize::new(0);
struct Demand;
impl Drop for Demand {fn drop(&mut self){DEMAND.fetch_sub(1,Ordering::Relaxed);}}
struct Worker {threads:usize,adaptive:bool,child:Child,connection:UnixStream,sequence:u64,tail:Arc<Mutex<VecDeque<u8>>>,stderr:Option<JoinHandle<()>>}
impl Drop for Worker {
 fn drop(&mut self){
  if matches!(self.child.try_wait(),Ok(None)){let _=self.child.kill();}
  let _=self.child.wait();if let Some(thread)=self.stderr.take(){let _=thread.join();}
 }
}
fn active(deadline:Instant)->Result<Duration>{
 ensure!(!super::cancelled(),"render request superseded");
 let left=deadline.saturating_duration_since(Instant::now());ensure!(!left.is_zero(),"packed worker deadline exceeded");Ok(left.min(POLL))
}
fn transfer(stream:&mut UnixStream,data:&mut [u8],write:bool,deadline:Instant)->Result<()> {
 let mut offset=0;
 while offset<data.len(){let timeout=active(deadline)?;
  if write{stream.set_write_timeout(Some(timeout))?;}else{stream.set_read_timeout(Some(timeout))?;}
  let result=if write{stream.write(&data[offset..])}else{stream.read(&mut data[offset..])};
  match result {
   Ok(0)=>bail!("packed worker closed its connection during a frame"),
   Ok(n)=>offset+=n,
   Err(error) if matches!(error.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut|std::io::ErrorKind::Interrupted)=>{},
   Err(error)=>return Err(error).context("packed worker frame IO failed"),
  }
 }Ok(())
}
fn send(stream:&mut UnixStream,mut data:&[u8],deadline:Instant)->Result<()> {
 while !data.is_empty(){stream.set_write_timeout(Some(active(deadline)?))?;
  match stream.write(data){Ok(0)=>bail!("packed worker closed during input"),Ok(n)=>data=&data[n..],
   Err(e) if matches!(e.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut|std::io::ErrorKind::Interrupted)=>{},
   Err(e)=>return Err(e).context("packed worker input IO failed"),}
 }Ok(())
}
impl Worker {
 fn launch()->Result<Self>{
  let path=std::env::var_os("RIVER_OPENHT_WORKER").unwrap_or_else(||"/usr/local/bin/river-openht-worker".into());
  let workers=number("RIVER_OPENHT_WORKERS",1).clamp(1,8);
  let cpus=std::thread::available_parallelism().map(|n|n.get()).unwrap_or(2);
  let direct=!std::env::var("RIVER_OPENHT_DIRECT_BLOCKS").is_ok_and(|v|v=="false" || v=="0");
  let cap=if direct {16}else{8};
  let automatic=if std::env::var("RIVER_PROFILE").as_deref()==Ok("compact") {2} else {(cpus.max(4)/2/workers).clamp(1,cap)};
  let threads=number("RIVER_OPENHT_WORKER_THREADS",automatic).clamp(1,16);let memory=number("RIVER_OPENHT_WORKER_MEMORY_MIB",1536).clamp(256,4096);
  // Negotiate optional frame budgets so older custom workers keep protocol1.
  let explicit=std::env::var("RIVER_OPENHT_WORKER_THREADS").ok().and_then(|v|v.parse::<usize>().ok()).is_some_and(|v|v>0);
  let adaptive=!explicit && threads>8 && Command::new(&path).arg("--frame-threads").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|v|v.success());
  let (connection,peer)=UnixStream::pair()?;let input:OwnedFd=peer.try_clone()?.into();let output:OwnedFd=peer.into();
  let mut command=Command::new(path);
  let allocator=std::env::var_os("RIVER_OPENHT_ALLOCATOR_LIBRARY").unwrap_or_else(||"/usr/lib/x86_64-linux-gnu/libmimalloc.so.2".into());
  if !allocator.is_empty() && std::path::Path::new(&allocator).is_file() {command.env("LD_PRELOAD",allocator);}
  let mut child=command.args([threads.to_string(),memory.to_string(),std::process::id().to_string()])
   .stdin(Stdio::from(input)).stdout(Stdio::from(output)).stderr(Stdio::piped()).spawn().context("cannot start packed decoder worker")?;
  let mut stderr=child.stderr.take().context("packed worker stderr unavailable")?;let tail=Arc::new(Mutex::new(VecDeque::new()));let capture=tail.clone();
  let reader=std::thread::Builder::new().name("openht-stderr".into()).spawn(move||{let mut buffer=[0u8;4096];while let Ok(n)=stderr.read(&mut buffer){if n==0{break;}
   let mut tail=capture.lock().unwrap_or_else(|e|e.into_inner());tail.extend(&buffer[..n]);while tail.len()>16384{tail.pop_front();}
  }});
  let reader=match reader{Ok(reader)=>reader,Err(error)=>{let _=child.kill();let _=child.wait();return Err(error).context("cannot capture packed worker diagnostics");}};
  tracing::info!(worker_pid=child.id(),threads,memory_mib=memory,"OpenHT worker started");
  Ok(Self{threads,adaptive,child,connection,sequence:0,tail,stderr:Some(reader)})
 }
 fn render(&mut self,input:&[u8],width:u32,height:u32,deadline:Instant,own_preparation:bool)->Result<image::RgbaImage>{
  ensure!(input.len()<=MAX_INPUT && width>0 && height>0 && u64::from(width)*u64::from(height)<=MAX_PIXELS,"packed worker input exceeds bounds");
  self.sequence=self.sequence.checked_add(1).context("packed worker sequence exhausted")?;let id=self.sequence;
  self.tail.lock().unwrap_or_else(|e|e.into_inner()).clear();
  let busy=DEMAND.load(Ordering::Relaxed)>1 || crate::source_admission::active_preparations()>usize::from(own_preparation);
  let budget=if self.adaptive {(if busy {self.threads.min(8)}else{self.threads}) as u32}else{0};
  let flags=if self.adaptive {0x54485231}else{0};
  let fields=[REQUEST,input.len() as u32,width,height,id as u32,(id>>32) as u32,budget,flags];let mut header=[0u8;32];
  for (n,v) in fields.iter().enumerate(){header[n*4..n*4+4].copy_from_slice(&v.to_le_bytes());}
  transfer(&mut self.connection,&mut header,true,deadline)?;
  send(&mut self.connection,input,deadline)?;
  transfer(&mut self.connection,&mut header,false,deadline)?;
  let values:Vec<u32>=header.chunks_exact(4).map(|b|u32::from_le_bytes(b.try_into().unwrap())).collect();
  ensure!(values[0]==RESPONSE,"packed worker returned an unsupported response protocol");
  ensure!((u64::from(values[5])|(u64::from(values[6])<<32))==id,"packed worker returned a stale response sequence");
  if values[1]!=0 {
   ensure!(values[1]==1 && values[4]<=4096,"invalid packed worker error response");let mut message=vec![0;values[4] as usize];transfer(&mut self.connection,&mut message,false,deadline)?;
   bail!("decoder rejected frame: {}",String::from_utf8_lossy(&message));
  }
  let size=usize::try_from(u64::from(width)*u64::from(height)*4)?;
  ensure!(values[2]==width && values[3]==height && values[4] as usize==size,"packed worker returned {}x{} ({}bytes); expected{}x{} ({}bytes)",values[2],values[3],values[4],width,height,size);
  ensure!(values[7]<=1,"packed worker returned invalid reuse flags");
  let mut rgba=vec![0;size];transfer(&mut self.connection,&mut rgba,false,deadline)?;
  if std::env::var("RIVER_PROFILE_SOURCE").is_ok_and(|v|v=="true") {tracing::info!(worker_pid=self.child.id(),sequence=id,reused=values[7]==1,width,height,"OpenHT packed LL render complete");}
  image::RgbaImage::from_raw(width,height,rgba).context("packed worker raster shape mismatch")
 }
 fn diagnostic(&self)->String {let tail=self.tail.lock().unwrap_or_else(|e|e.into_inner());String::from_utf8_lossy(&tail.iter().copied().collect::<Vec<_>>()).chars().rev().take(4096).collect::<String>().chars().rev().collect()}
 fn retire(mut self)->String {
  let pid=self.child.id();if matches!(self.child.try_wait(),Ok(None)){let _=self.child.kill();}
  let status=self.child.wait().map(|s|s.to_string()).unwrap_or_else(|e|e.to_string());
  if let Some(reader)=self.stderr.take(){let _=reader.join();}
  let diagnostic=self.diagnostic();if diagnostic.is_empty(){format!("packed worker {pid} ended ({status})")}else{format!("packed worker {pid} ended ({status}): {diagnostic}")}
 }
}
struct Spawn {reply:mpsc::SyncSender<Result<Worker>>,deadline:Instant}
type Supervisor=Arc<mpsc::SyncSender<Spawn>>;
fn supervisor_state()->&'static Mutex<Option<Supervisor>> {static STATE:OnceLock<Mutex<Option<Supervisor>>>=OnceLock::new();STATE.get_or_init(||Mutex::new(None))}
fn supervisor()->Result<Supervisor>{
 let mut state=supervisor_state().lock().unwrap_or_else(|e|e.into_inner());if let Some(sender)=&*state{return Ok(sender.clone());}
 let (sender,receiver)=mpsc::sync_channel::<Spawn>(8);
 // Linux parent-death signals follow the spawning thread. This thread must
 // outlive Tokio's temporary blocking workers and all decoder children.
 std::thread::Builder::new().name("openht-supervisor".into()).spawn(move||{
  while let Ok(job)=receiver.recv(){let result=if Instant::now()>=job.deadline {Err(anyhow::anyhow!("packed worker spawn deadline exceeded"))}else{Worker::launch()};let _=job.reply.send(result);}
 }).context("cannot start packed decoder supervisor")?;
 let sender=Arc::new(sender);*state=Some(sender.clone());Ok(sender)
}
fn spawn(deadline:Instant)->Result<Worker>{
 let sender=supervisor()?;let (reply,receiver)=mpsc::sync_channel(1);
 let mut job=Spawn{reply,deadline};loop{let timeout=active(deadline)?;match sender.try_send(job){
  Ok(())=>break,
  Err(mpsc::TrySendError::Full(returned))=>{job=returned;std::thread::sleep(timeout.min(Duration::from_millis(10)));},
  Err(mpsc::TrySendError::Disconnected(_))=>{let mut state=supervisor_state().lock().unwrap_or_else(|e|e.into_inner());if state.as_ref().is_some_and(|current|Arc::ptr_eq(current,&sender)){*state=None;}bail!("packed decoder supervisor stopped; retry the request");},
 }}
 loop {let timeout=active(deadline)?;match receiver.recv_timeout(timeout){Ok(result)=>return result,Err(mpsc::RecvTimeoutError::Timeout)=>{},Err(mpsc::RecvTimeoutError::Disconnected)=>bail!("packed decoder supervisor stopped while spawning"),}}
}
struct Slot {worker:Option<Worker>}
struct Pool {available:Mutex<Vec<Slot>>,ready:Condvar}
struct Lease {pool:Arc<Pool>,slot:Option<Slot>}
impl Drop for Lease {fn drop(&mut self){if let Some(slot)=self.slot.take(){self.pool.available.lock().unwrap_or_else(|e|e.into_inner()).push(slot);self.pool.ready.notify_one();}}}
fn number(name:&str,default:usize)->usize {std::env::var(name).ok().and_then(|v|v.parse().ok()).filter(|&v|v>0).unwrap_or(default)}
impl Pool {
 fn shared()->Arc<Self>{static POOL:OnceLock<Arc<Pool>>=OnceLock::new();POOL.get_or_init(||Arc::new(Pool{available:Mutex::new((0..number("RIVER_OPENHT_WORKERS",1).clamp(1,8)).map(|_|Slot{worker:None}).collect()),ready:Condvar::new()})).clone()}
 fn take(self:&Arc<Self>,deadline:Instant)->Result<Lease>{let mut available=self.available.lock().unwrap_or_else(|e|e.into_inner());loop{
  let timeout=active(deadline)?;if let Some(slot)=available.pop(){return Ok(Lease{pool:self.clone(),slot:Some(slot)});}
  available=self.ready.wait_timeout(available,timeout).unwrap_or_else(|e|e.into_inner()).0;
 }}
}
pub(super) fn decode(input:&[u8],width:u32,height:u32)->Result<image::RgbaImage>{decode_inner(input,width,height,false)}
pub(super) fn decode_preparing(input:&[u8],width:u32,height:u32)->Result<image::RgbaImage>{decode_inner(input,width,height,true)}
fn decode_inner(input:&[u8],width:u32,height:u32,own_preparation:bool)->Result<image::RgbaImage>{
 let deadline=Instant::now()+Duration::from_millis(number("RIVER_OPENHT_TIMEOUT_MS",30000).clamp(50,60000) as u64);
 DEMAND.fetch_add(1,Ordering::Relaxed);let _demand=Demand;
 let mut lease=Pool::shared().take(deadline)?;let slot=lease.slot.as_mut().unwrap();
 let exited=match &mut slot.worker {Some(worker)=>worker.child.try_wait().context("cannot inspect packed worker")?.is_some(),None=>false};
 if exited {slot.worker.take();}
 if slot.worker.is_none(){slot.worker=Some(spawn(deadline)?);}
 let result=slot.worker.as_mut().unwrap().render(input,width,height,deadline,own_preparation);
 match result {Ok(image)=>Ok(image),Err(error)=>{let diagnostic=slot.worker.take().unwrap().retire();Err(error.context(diagnostic))}}
}
