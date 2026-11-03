//! Raw SDK replay matching River's synchronous range-reader call pattern.
use anyhow::{Result,ensure,Context};
use futures_util::TryStreamExt;
use object_store::{ObjectStore,GetOptions,GetRange,aws::AmazonS3Builder,path::Path};
use sha2::{Digest,Sha256};
use std::{io::Read,time::{Duration,Instant}};
fn cpu_ms()->f64 {let mut t=libc::timespec {tv_sec:0,tv_nsec:0};unsafe {libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID,&mut t)};t.tv_sec as f64*1000.0+t.tv_nsec as f64/1e6}
fn main()->Result<()> {
 let mut input=String::new();std::io::stdin().read_to_string(&mut input)?;let mut lines=input.lines();
 let url=lines.next().context("missing fixture URL")?;ensure!(url.starts_with("http://172.17."),"fixture-only tool");
 let etag=lines.next().context("missing ETag")?.to_owned();
 let remaining=lines.collect::<Vec<_>>().join(" ");let mut words=remaining.split_whitespace();
 let mut number=||->Result<u64>{Ok(words.next().context("missing range number")?.parse()?)};
 let size=number()?;let count=number()?;ensure!(count>0 && count<=20000,"invalid range count");let mut ranges=vec![];
 for _ in 0..count {let a=number()?;let b=number()?;ensure!(a<=b && b<size && b-a<64*1024*1024,"invalid replay range");ranges.push(a..b+1);}
 let (endpoint,key)=url.split_once("/fixture/").context("fixture URL missing bucket")?;
 let store=AmazonS3Builder::new().with_bucket_name("fixture").with_region("us-east-1")
  .with_endpoint(endpoint).with_allow_http(true).with_virtual_hosted_style_request(false)
  .with_access_key_id("fixture").with_secret_access_key("fixture").build()?;
 let location=Path::from(key);let runtime=tokio::runtime::Runtime::new()?;
 let (_cancel_tx,cancel)=tokio::sync::watch::channel(false);let mut digest=Sha256::new();let mut bytes=0u64;
 let started=Instant::now();let cpu=cpu_ms();
 for range in ranges {
  let mut cancel=cancel.clone();let opts=GetOptions {range:Some(GetRange::Bounded(range.clone())),if_match:Some(etag.clone()),..Default::default()};
  let body=runtime.handle().block_on(async {
   let get=async {
    let result=store.get_opts(&location,opts).await?;ensure!(result.range==range && result.meta.size==size,"range response changed");
    ensure!(result.meta.e_tag.as_deref()==Some(etag.as_str()),"ETag changed");
    let mut stream=result.into_stream();let mut bytes=Vec::with_capacity((range.end-range.start) as usize);
    while let Some(chunk)=stream.try_next().await? {ensure!(bytes.len() as u64+chunk.len() as u64<=range.end-range.start,"oversized response");bytes.extend_from_slice(&chunk);}
    ensure!(bytes.len() as u64==range.end-range.start,"truncated response");Ok::<_,anyhow::Error>(bytes)
   };
   tokio::select! {r=tokio::time::timeout(Duration::from_secs(10),get)=>r.context("range timeout")?,_=cancel.changed()=>anyhow::bail!("cancelled")}
  })?;
  bytes+=body.len() as u64;digest.update(&body);
 }
 let elapsed=started.elapsed().as_secs_f64()*1000.0;let cpu=cpu_ms()-cpu;
 println!("{}",serde_json::json!({"passed":true,"wallMs":elapsed,"processCpuMs":cpu,"gets":count,"bytes":bytes,"sha256":format!("{:x}",digest.finalize())}));
 Ok(())
}
