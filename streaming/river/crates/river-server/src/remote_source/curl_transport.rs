//! Experimental serial range transport. Batched ranges retain the SDK path.
use super::*;
use std::{ffi::{c_char,c_int,c_void,CStr,CString},ptr::NonNull,sync::OnceLock};
use object_store::{aws::{AmazonS3,AwsCredentialProvider},signer::Signer};
#[repr(C)]
struct Request {url:*const c_char,signing:*const c_char,userpwd:*const c_char,token:*const c_char,etag:*const c_char,version:*const c_char,ca_info:*const c_char,start:u64,end:u64,total:u64,timeout_ms:u64}
#[repr(C)] #[derive(Default)] struct Response {written:u64,status:i64,curl_code:i32,violation:i32,requests:u64,retry_after_ms:u64}
#[derive(Debug)] pub(super) struct Failure {pub requests:u64,pub bytes:u64,pub retry_after_ms:u64,pub retryable:bool,message:String}
impl std::fmt::Display for Failure {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(&self.message)}}
impl std::error::Error for Failure {}
pub(super) struct Fetched {pub bytes:Vec<u8>,pub requests:u64}
type Cancel=unsafe extern "C" fn(*mut c_void)->c_int;
type Fetch=unsafe extern "C" fn(*mut c_void,*const Request,*mut u8,usize,Cancel,*mut c_void,*mut Response)->c_int;
struct Api {_library:libloading::Library,create:unsafe extern "C" fn()->*mut c_void,destroy:unsafe extern "C" fn(*mut c_void),fetch:Fetch,error:unsafe extern "C" fn(c_int)->*const c_char}
fn api()->Result<&'static Api> {
 static API:OnceLock<std::result::Result<Api,String>>=OnceLock::new();
 API.get_or_init(||unsafe {
  (||->Result<Api>{
   let library=libloading::Library::new(std::env::var("RIVER_CURL_LIBRARY").unwrap_or_else(|_|"/usr/local/lib/libriver_curl.so".into())).context("native S3 curl library unavailable")?;
   let abi=*library.get::<unsafe extern "C" fn()->u32>(b"river_curl_abi\0")?;
   let request_size=*library.get::<unsafe extern "C" fn()->u32>(b"river_curl_request_size\0")?;
   let response_size=*library.get::<unsafe extern "C" fn()->u32>(b"river_curl_response_size\0")?;
   ensure!(abi()==3 && request_size() as usize==std::mem::size_of::<Request>() && response_size() as usize==std::mem::size_of::<Response>(),"native S3 curl ABI mismatch");
   Ok(Api {create:*library.get(b"river_curl_create\0")?,destroy:*library.get(b"river_curl_destroy\0")?,fetch:*library.get(b"river_curl_fetch\0")?,error:*library.get(b"river_curl_error_text\0")?,_library:library})
  })().map_err(|e|format!("{e:#}"))
 }).as_ref().map_err(|s|anyhow::anyhow!(s.clone()))
}
pub(super) fn initialize()->Result<()> {
 for name in ["HTTP_PROXY","HTTPS_PROXY","ALL_PROXY","http_proxy","https_proxy","all_proxy","SSL_CERT_DIR"] {ensure!(std::env::var_os(name).is_none_or(|v|v.is_empty()),"native S3 curl does not support {name}; use sdk transport");}
 let api=api()?;let p=unsafe{(api.create)()};ensure!(!p.is_null(),"native S3 curl requires libcurl7.85+, asynchronous DNS, and available handle memory");unsafe{(api.destroy)(p)};Ok(())
}
pub(super) struct Source {store:Arc<AmazonS3>,region:String,allow_http:bool,ready:Mutex<Option<Arc<Config>>>}
impl std::fmt::Debug for Source {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.debug_struct("NativeS3Source").finish_non_exhaustive()}}
impl Source {
 pub(super) fn new(store:Arc<AmazonS3>,region:String,allow_http:bool)->Self {Self {store,region,allow_http,ready:Mutex::new(None)}}
 pub(super) fn config(&self,path:&Path,meta:&ObjectMeta,runtime:&tokio::runtime::Handle)->Result<Arc<Config>> {
  let mut slot=self.ready.lock().unwrap_or_else(|e|e.into_inner());if let Some(v)=slot.as_ref(){return Ok(v.clone());}
  let value=Arc::new(runtime.block_on(Config::new(self.store.as_ref(),path,meta,&self.region,self.allow_http))?);*slot=Some(value.clone());Ok(value)
 }
}
struct Handle {ptr:NonNull<c_void>,api:&'static Api}
// libcurl permits moving an exclusively owned handle between threads. No
// callbacks or active transfer survive fetch(); the pool never shares a lease.
unsafe impl Send for Handle {}
impl Drop for Handle {fn drop(&mut self){unsafe{(self.api.destroy)(self.ptr.as_ptr())}}}
#[derive(Default)] struct Pool {idle:Vec<Handle>,created:usize}
fn pool()->&'static Mutex<Pool>{static POOL:OnceLock<Mutex<Pool>>=OnceLock::new();POOL.get_or_init(||Mutex::new(Pool::default()))}
pub(super) struct Lease(Option<Handle>);
impl Drop for Lease {fn drop(&mut self){if let Some(handle)=self.0.take(){pool().lock().unwrap_or_else(|e|e.into_inner()).idle.push(handle);}}}
pub(super) fn lease()->Result<Option<Lease>> {
 let mut p=pool().lock().unwrap_or_else(|e|e.into_inner());
 if let Some(handle)=p.idle.pop(){return Ok(Some(Lease(Some(handle))));}
 if p.created>=16{return Ok(None);} // bounded, nonblocking SDK fallback
 let api=api()?;let ptr=NonNull::new(unsafe{(api.create)()}).context("cannot allocate native S3 curl handle")?;
 p.created+=1;Ok(Some(Lease(Some(Handle{ptr,api}))))
}
pub(super) struct Config {url:CString,signing:CString,etag:Option<CString>,version:Option<CString>,ca_info:Option<CString>,credentials:AwsCredentialProvider,total:u64}
impl std::fmt::Debug for Config {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.debug_struct("NativeS3Transport").field("source_bytes",&self.total).finish_non_exhaustive()}}
fn value(name:&str,s:&str)->Result<CString>{ensure!(!s.bytes().any(|b|b==b'\r'||b==b'\n'),"native S3 {name} contains invalid header characters");CString::new(s).with_context(||format!("native S3 {name} contains a NUL byte"))}
impl Config {
 pub(super) async fn new(store:&AmazonS3,path:&Path,meta:&ObjectMeta,region:&str,allow_http:bool)->Result<Self>{
  ensure!(!region.is_empty() && region.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'),"native S3 curl does not support this region spelling");
  for name in ["HTTP_PROXY","HTTPS_PROXY","ALL_PROXY","http_proxy","https_proxy","all_proxy","SSL_CERT_DIR"] {
   ensure!(std::env::var_os(name).is_none_or(|v|v.is_empty()),"native S3 curl prototype does not support {name}; use sdk transport");
  }
  // Let the SDK construct the bucket/path style and escaping. Discard its
  // temporary query signature; libcurl signs each actual conditional GET.
  let mut url=store.signed_url(axum::http::Method::GET,path,Duration::from_secs(60)).await?;url.set_query(None);
  ensure!(url.scheme()=="https" || (allow_http && url.scheme()=="http"),"native S3 curl URL violates the source transport policy");
  if let Some(version)=&meta.version{url.query_pairs_mut().append_pair("versionId",version);}
  let ca=std::env::var("CURL_CA_BUNDLE").ok().or_else(||std::env::var("SSL_CERT_FILE").ok()).filter(|s|!s.is_empty());
  Ok(Self {url:value("URL",url.as_str())?,signing:value("region",&format!("aws:amz:{region}:s3"))?,etag:meta.e_tag.as_deref().map(|s|value("ETag",s)).transpose()?,version:meta.version.as_deref().map(|s|value("version",s)).transpose()?,ca_info:ca.as_deref().map(|s|value("CA file",s)).transpose()?,credentials:store.credentials().clone(),total:meta.size})
 }
}
struct Cancellation {deadline:Instant,receiver:Option<watch::Receiver<bool>>}
unsafe extern "C" fn cancelled(p:*mut c_void)->c_int {
 // Never unwind across the C boundary, including if a watch lock is poisoned.
 std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
  let c=unsafe{&*p.cast::<Cancellation>()};Instant::now()>=c.deadline || c.receiver.as_ref().is_some_and(|r|*r.borrow() || r.has_changed().is_err())
 })).unwrap_or(true) as c_int
}
impl Lease {
 pub(super) fn fetch(&mut self,config:&Config,runtime:&tokio::runtime::Handle,range:Range<u64>,deadline:Instant,receiver:Option<watch::Receiver<bool>>)->Result<Fetched> {
  ensure!(range.start<range.end && range.end<=config.total,"invalid native S3 range extent");
  let mut credential_cancel=receiver.clone();let remaining=deadline.saturating_duration_since(Instant::now());
  let credentials=runtime.block_on(async {
   let stopped=async {if let Some(r)=&mut credential_cancel {if !*r.borrow(){let _=r.changed().await;}}else{std::future::pending::<()>().await;}};
   tokio::select! {r=tokio::time::timeout(remaining.min(Duration::from_secs(10)),config.credentials.get_credential())=>r.context("S3 credentials timed out")?.map_err(anyhow::Error::from),_=stopped=>bail!("S3 request superseded")}
  })?;
  ensure!(!credentials.key_id.contains(':'),"native S3 curl does not support a colon in the credential identifier");
  let userpwd=value("credentials",&format!("{}:{}",credentials.key_id,credentials.secret_key))?;
  let token=credentials.token.as_deref().map(|s|value("session token",s)).transpose()?;
  let pointer=|v:&Option<CString>|v.as_ref().map_or(std::ptr::null(),|s|s.as_ptr());
  let length=usize::try_from(range.end-range.start)?;ensure!(length>0 && length<=1024*1024,"native S3 serial range exceeds its1MiB buffer limit");
  let timeout_ms=deadline.saturating_duration_since(Instant::now()).as_millis().min(10000) as u64;ensure!(timeout_ms>0,"S3 range-read deadline exceeded");
  let request=Request {url:config.url.as_ptr(),signing:config.signing.as_ptr(),userpwd:userpwd.as_ptr(),token:pointer(&token),etag:pointer(&config.etag),version:pointer(&config.version),ca_info:pointer(&config.ca_info),start:range.start,end:range.end-1,total:config.total,timeout_ms};
  let mut cancellation=Cancellation {deadline,receiver};let mut bytes=Vec::<u8>::with_capacity(length);let mut response=Response::default();let handle=self.0.as_ref().unwrap();
  let result=unsafe{(handle.api.fetch)(handle.ptr.as_ptr(),&request,bytes.as_mut_ptr(),length,cancelled,(&mut cancellation as *mut Cancellation).cast(),&mut response)};
  if result!=0 {
   let stopped=|message:&str|->anyhow::Error {Failure {requests:response.requests,bytes:response.written,retry_after_ms:0,retryable:false,message:message.into()}.into()};
   if result==1 {
    if cancellation.receiver.as_ref().is_some_and(|r|*r.borrow() || r.has_changed().is_err()){return Err(stopped("S3 request superseded"));}
    if Instant::now()>=deadline {return Err(stopped("S3 range-read deadline exceeded"));}
    return Err(stopped("native S3 cancellation check failed"));
   }
   if result==2 {return Err(stopped(if Instant::now()>=deadline {"S3 range-read deadline exceeded"}else{"S3 native range timed out"}));}
   let reason=match result {
    3=>match response.status {200=>"S3 endpoint ignored the requested byte range",301|302|303|307|308=>"S3 endpoint redirected the range; check the source endpoint and region",401|403=>"S3 source denied access to the requested object",404=>"S3 object was not found",412=>"S3 object changed since its metadata was read",416=>"S3 endpoint rejected the requested byte extent",429|503=>"S3 source is throttled or temporarily unavailable",_=>"S3 native range returned an HTTP error"},
    4=>match response.violation {1=>"S3 response has a missing or mismatched Content-Range",2=>"S3 response has a missing or changed ETag",3=>"S3 response has a missing or changed version ID",4=>"S3 response Content-Length does not match the requested range",5=>"S3 response exceeded the requested byte extent",6=>"S3 response ended before the requested range was complete",_=>"S3 response violated its range contract"},
    6=>"invalid native S3 range arguments",_=>"native S3 transport failed"};
   let retryable=matches!(response.status,408|429|500|502|503|504) || (result==4 && response.violation==6) || (result==5 && matches!(response.curl_code,6|7|18|52|55|56));
   if result==3 || result==4 {return Err(Failure {requests:response.requests,bytes:response.written,retry_after_ms:response.retry_after_ms,retryable,message:format!("{reason} (HTTP {})",response.status)}.into());}
   let detail=unsafe{CStr::from_ptr((handle.api.error)(response.curl_code))}.to_string_lossy();
   return Err(Failure {requests:response.requests,bytes:response.written,retry_after_ms:response.retry_after_ms,retryable,message:format!("{reason} (HTTP {}, curl {}: {detail})",response.status,response.curl_code)}.into());
  }
  ensure!(response.written==length as u64,"native S3 response length mismatch");
  // The bridge writes only within this capacity and reports success only
  // after receiving exactly length initialized bytes (fault-tested separately).
  unsafe{bytes.set_len(length)};Ok(Fetched {bytes,requests:response.requests})
 }
}
