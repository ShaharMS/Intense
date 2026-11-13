//! Explicit private configuration storage, separate from disposable caches.
use super::{AppState, S3Source, source_from_input};
use anyhow::{Result, ensure};
use std::collections::HashMap;
use object_store::client::{ClientOptions,HttpClient,HttpConnector,ReqwestConnector,HttpService,HttpRequest,HttpResponse,HttpError};
#[derive(Debug)]struct TracedService(HttpClient);
#[async_trait::async_trait]
impl HttpService for TracedService {
 async fn call(&self,request:HttpRequest)->std::result::Result<HttpResponse,HttpError> {
  crate::remote_source::wire_budget::before_send().map_err(|e|HttpError::new(object_store::client::HttpErrorKind::Unknown,e))?;
  let mut trace=crate::remote_source::read_trace::Read::wire();
  let response=self.0.execute(request).await;
  trace.headers(response.as_ref().ok().map(|r|r.status().as_u16()),response.is_ok());response
 }
}

#[derive(Debug,Clone)]
pub(super) struct SharedTransport {client:HttpClient,profile:String}
impl HttpConnector for SharedTransport {
    fn connect(&self,options:&ClientOptions)->object_store::Result<HttpClient> {
        // SDK-internal metadata/session clients may impose stricter transport
        // settings. Never replace those with this source's pooled policy.
        if format!("{options:?}")==self.profile {Ok(HttpClient::new(TracedService(self.client.clone())))}
        else {ReqwestConnector::default().connect(options)}
    }
}

pub(super) fn transport(allow_http:bool)->Result<(ClientOptions,SharedTransport)> {
    // All source transport options are constructed here. Credentials/signing
    // stay on each S3 store, never in this client. If per-source TLS/proxy/client
    // headers are added, they must become part of this transport policy key.
    static POOLS:std::sync::OnceLock<std::sync::Mutex<[Option<HttpClient>;2]>>=std::sync::OnceLock::new();
    let options=ClientOptions::new().with_allow_http(allow_http);
    let mut pools=POOLS.get_or_init(||std::sync::Mutex::new([None,None])).lock().unwrap_or_else(|error|error.into_inner());
    let client=match &pools[usize::from(allow_http)] {
        Some(client)=>client.clone(),
        None=>{let client=ReqwestConnector::default().connect(&options)?;pools[usize::from(allow_http)]=Some(client.clone());client}
    };
    let profile=format!("{options:?}");
    Ok((options,SharedTransport {client,profile}))
}

pub(super) fn save(sources:&HashMap<String,S3Source>)->Result<()> {
    let mut values=sources.values().map(|s|s.connection.clone()).collect::<Vec<_>>();values.sort_by(|a,b|a.id.cmp(&b.id));
    // Preserve in-memory-only source configuration for unconfigured development servers.
    if !crate::configuration::env_only() && std::env::var_os("RIVER_CONFIGURATION_FILE").is_none() && std::env::var_os("RIVER_S3_CONNECTIONS_FILE").is_none() {return Ok(());}
    crate::configuration::update(|d|{d.sources=values;Ok(())})?;Ok(())
}
pub(super) async fn restore(state:&AppState)->Result<()> {
    let values=crate::configuration::snapshot()?.sources;let mut restored=HashMap::new();
    for value in values {let source=source_from_input(&value)?;ensure!(restored.insert(source.id.clone(),source).is_none(),"Duplicate S3 connection");}
    *state.s3_sources.write().await=restored;Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use object_store::{ObjectStore,ObjectStoreExt,aws::AmazonS3Builder,path::Path};
    #[tokio::test]
    async fn configured_sdk_retry_limits_bound_physical_attempts() {
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        #[derive(Debug)]struct Unavailable(Arc<AtomicUsize>);
        #[async_trait::async_trait]impl HttpService for Unavailable {
            async fn call(&self,_:HttpRequest)->std::result::Result<HttpResponse,HttpError> {
                self.0.fetch_add(1,Ordering::Relaxed);
                Ok(axum::http::Response::builder().status(503)
                    .body(object_store::client::HttpResponseBody::from(Vec::<u8>::new())).unwrap())
            }
        }
        #[derive(Debug)]struct Connector(HttpClient);
        impl HttpConnector for Connector {fn connect(&self,_:&ClientOptions)->object_store::Result<HttpClient> {Ok(HttpClient::new(TracedService(self.0.clone())))}}
        for retries in [0,1,2] {
            let calls=Arc::new(AtomicUsize::new(0));
            let store=AmazonS3Builder::new().with_region("us-east-1").with_bucket_name("fixture")
                .with_access_key_id("fixture").with_secret_access_key("fixture")
                .with_http_connector(Connector(HttpClient::new(Unavailable(calls.clone()))))
                .with_retry(object_store::RetryConfig {max_retries:retries,..Default::default()}).build().unwrap();
            assert!(store.get_opts(&Path::from("image.jp2"),object_store::GetOptions {range:Some(object_store::GetRange::Bounded(0..3)),..Default::default()}).await.is_err());
            assert_eq!(calls.load(Ordering::Relaxed),retries+1);
            for limit in [0,1] {
                calls.store(0,Ordering::Relaxed);
                let budget=crate::remote_source::wire_budget::Budget::new(limit);
                let result=budget.scope(store.get_opts(&Path::from("image.jp2"),object_store::GetOptions {range:Some(object_store::GetRange::Bounded(0..3)),..Default::default()})).await;
                assert!(result.is_err());
                assert_eq!(calls.load(Ordering::Relaxed),limit as usize,"SDK retries must stop before another send");
            }
        }
    }
    #[tokio::test]
    async fn sdk_retries_are_visible_as_separate_transport_attempts() {
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        use crate::remote_source::read_trace::{self,Read};
        #[derive(Debug)]struct Mock(Arc<AtomicUsize>);
        #[async_trait::async_trait]impl HttpService for Mock {
            async fn call(&self,_:HttpRequest)->std::result::Result<HttpResponse,HttpError> {
                let n=self.0.fetch_add(1,Ordering::Relaxed);
                Ok(axum::http::Response::builder().status(if n==0 {503}else{206})
                    .header("Content-Range","bytes 0-2/3").header("Content-Length","3")
                    .header("ETag","\"test\"").header("Last-Modified","Wed, 21 Oct 2015 07:28:00 GMT")
                    .body(object_store::client::HttpResponseBody::from(vec![1,2,3])).unwrap())
            }
        }
        #[derive(Debug)]struct Connector(HttpClient);
        impl HttpConnector for Connector {fn connect(&self,_:&ClientOptions)->object_store::Result<HttpClient> {Ok(HttpClient::new(TracedService(self.0.clone())))}}
        let calls=Arc::new(AtomicUsize::new(0));let store=AmazonS3Builder::new().with_region("us-east-1").with_bucket_name("fixture")
            .with_access_key_id("fixture").with_secret_access_key("fixture").with_http_connector(Connector(HttpClient::new(Mock(calls.clone())))).build().unwrap();
        read_trace::configure(true);let id=read_trace::context_id();
        let mut trace=Read::begin(id,"opaque",std::panic::Location::caller(),"test",0..3,1,None,None,"sdk");
        let response=trace.scope(store.get_opts(&Path::from("image.jp2"),object_store::GetOptions {range:Some(object_store::GetRange::Bounded(0..3)),..Default::default()})).await.unwrap();
        assert_eq!(&response.bytes().await.unwrap()[..],&[1,2,3]);trace.finish(3,true,None);drop(trace);
        let value=read_trace::snapshot();let attempts=value["events"].as_array().unwrap().iter().filter(|e|e["contextId"]==id && e["kind"]=="wireAttempt").collect::<Vec<_>>();
        assert_eq!(calls.load(Ordering::Relaxed),2);assert_eq!(attempts.len(),2);assert_eq!(attempts[0]["httpStatus"],503);assert_eq!(attempts[1]["httpStatus"],206);
        read_trace::configure(false);
    }
    #[tokio::test]
    async fn sdk_https_policy_is_not_weakened_by_a_shared_http_transport() {
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (_,connector)=transport(true).unwrap();
        let store=AmazonS3Builder::new().with_region("us-east-1").with_bucket_name("fixture")
            .with_access_key_id("fixture").with_secret_access_key("fixture")
            .with_endpoint(format!("http://{}",listener.local_addr().unwrap()))
            .with_client_options(ClientOptions::new().with_allow_http(false))
            .with_http_connector(connector).build().unwrap();
        let result=tokio::time::timeout(std::time::Duration::from_secs(2),store.head(&Path::from("image.jp2"))).await;
        assert!(result.expect("HTTPS-only rejection must not wait on network").is_err());
        assert!(matches!(listener.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),
            "SDK HTTPS-only request reached the HTTP listener");
    }
}
