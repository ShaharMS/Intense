//! One private atomic configuration document; legacy files are read for migration.
use std::{io::{Read,Write},path::PathBuf,sync::{Mutex,OnceLock}};
use anyhow::{Result,ensure,Context};
use super::{S3SourceInput,encoding_settings,read_policy};
#[derive(Clone,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Document {pub schema_version:u32,pub encoding:encoding_settings::Settings,pub read_policy:read_policy::Settings,pub sources:Vec<S3SourceInput>}
impl Document {
 pub fn validate(&self)->Result<()> {
  ensure!(self.schema_version==1,"schemaVersion must be 1");ensure!(self.encoding.webp_method<=6,"webpMethod must be 0 through 6");self.read_policy.validate()?;
  ensure!(self.sources.len()<=100,"At most 100 sources are supported");let mut ids=std::collections::HashSet::new();
  for s in &self.sources {ensure!(ids.insert(&s.id),"Duplicate source ID");ensure!(s.access_key_id!="[redacted]" && s.secret_access_key!="[redacted]" && s.session_token.as_deref()!=Some("[redacted]"),"A redacted export cannot be imported; export with includeSecrets=true");}
  Ok(())
 }
}
fn path()->Option<PathBuf> {std::env::var_os("RIVER_CONFIGURATION_FILE").map(PathBuf::from).or_else(||std::env::var_os("RIVER_S3_CONNECTIONS_FILE").and_then(|p|PathBuf::from(p).parent().map(|p|p.join("configuration.json")))).or_else(||std::env::var_os("RIVER_ENCODING_SETTINGS_FILE").and_then(|p|PathBuf::from(p).parent().map(|p|p.join("configuration.json"))))}
fn read_file<T:serde::de::DeserializeOwned>(path:&std::path::Path)->Result<Option<T>> {
 let f=match std::fs::File::open(path){Ok(f)=>f,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e.into())};
 let mut bytes=vec![];f.take(1024*1024+1).read_to_end(&mut bytes)?;ensure!(bytes.len()<=1024*1024,"Configuration exceeds 1 MiB");Ok(Some(serde_json::from_slice(&bytes)?))
}
fn state()->&'static Mutex<Option<Document>> {static STATE:OnceLock<Mutex<Option<Document>>>=OnceLock::new();STATE.get_or_init(||Mutex::new(None))}
fn load()->Result<Document> {
 if let Some(p)=path() {if let Some(d)=read_file::<Document>(&p)? {d.validate()?;return Ok(d);}}
 let mut d=Document {schema_version:1,encoding:encoding_settings::Settings {webp_method:encoding_settings::snapshot()},read_policy:read_policy::Settings::default(),sources:vec![]};
 if let Some(p)=std::env::var_os("RIVER_S3_CONNECTIONS_FILE").map(PathBuf::from) {
  if let Some(s)=read_file(&p)? {d.sources=s;}
  if let Some(parent)=p.parent() {let encoding=std::env::var_os("RIVER_ENCODING_SETTINGS_FILE").map(PathBuf::from).unwrap_or_else(||parent.join("encoding.json"));if let Some(s)=read_file(&encoding)? {d.encoding=s;}}
 } else if let Some(p)=std::env::var_os("RIVER_ENCODING_SETTINGS_FILE").map(PathBuf::from) {if let Some(s)=read_file(&p)? {d.encoding=s;}}
 d.validate()?;Ok(d)
}
pub fn snapshot()->Result<Document> {let mut s=state().lock().unwrap_or_else(|e|e.into_inner());if s.is_none(){*s=Some(load()?);}Ok(s.as_ref().unwrap().clone())}
pub fn update(change:impl FnOnce(&mut Document)->Result<()>)->Result<Document> {
 let mut state=state().lock().unwrap_or_else(|e|e.into_inner());let mut next=match state.as_ref(){Some(s)=>s.clone(),None=>load()?};change(&mut next)?;next.validate()?;
 let bytes=serde_json::to_vec(&next)?;ensure!(bytes.len()<=1024*1024,"Configuration exceeds 1 MiB");let p=path().context("Configure RIVER_CONFIGURATION_FILE or RIVER_S3_CONNECTIONS_FILE for persistent settings")?;
 let parent=p.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));std::fs::create_dir_all(parent)?;
 let mut file=tempfile::NamedTempFile::new_in(parent)?;file.write_all(&bytes)?;file.as_file().sync_all()?;file.persist(p)?;*state=Some(next.clone());Ok(next)
}
#[cfg(test)] mod tests {
 use super::*;
 fn document()->serde_json::Value {serde_json::json!({"schemaVersion":1,"encoding":{"webpMethod":0},"readPolicy":{"mode":"observe","minimumSamples":16,"maxOverfetchRatio":2},"sources":[]})}
 #[test] fn versioned_document_rejects_unknown_fields_and_future_versions() {let mut d=document();d["schemaVersion"]=2.into();assert!(serde_json::from_value::<Document>(d.clone()).unwrap().validate().is_err());d["schemaVersion"]=1.into();d["typo"]=true.into();assert!(serde_json::from_value::<Document>(d).is_err());}
 #[test] fn redacted_exports_and_duplicate_sources_cannot_replace_configuration() {let mut d=document();let s=serde_json::json!({"id":"test","name":"test","bucket":"test","accessKeyId":"key","secretAccessKey":"[redacted]"});d["sources"]=serde_json::json!([s.clone()]);assert!(serde_json::from_value::<Document>(d.clone()).unwrap().validate().is_err());d["sources"]=serde_json::json!([s.clone(),s]);d["sources"][0]["secretAccessKey"]="secret".into();d["sources"][1]["secretAccessKey"]="secret".into();assert!(serde_json::from_value::<Document>(d).unwrap().validate().is_err());}
}
