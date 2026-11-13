//! One private atomic configuration document; legacy files are read for migration.
use std::{io::{Read,Write},path::PathBuf,sync::{Mutex,OnceLock}};
use anyhow::{Result,ensure,Context};
use super::{S3SourceInput,encoding_settings,read_policy};
#[derive(Clone,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Document {pub schema_version:u32,pub encoding:encoding_settings::Settings,pub read_policy:read_policy::Settings,pub sources:Vec<S3SourceInput>,#[serde(default,skip_serializing_if="Option::is_none")] pub enabled_drivers:Option<Vec<super::DecoderDriver>>,#[serde(default,skip_serializing_if="Option::is_none")] pub tls:Option<super::tls::Settings>,#[serde(default,skip_serializing_if="Option::is_none")] pub logging:Option<super::console_log::Settings>,#[serde(default,skip_serializing_if="Option::is_none")] pub auto_adaptation_mode:Option<String>,#[serde(default,skip_serializing_if="Option::is_none")] pub shared_cache:Option<super::shared_cache::Settings>}
impl Document {
 pub fn validate(&self)->Result<()> {
  ensure!(self.schema_version==1,"schemaVersion must be 1");ensure!(self.encoding.webp_method<=6,"webpMethod must be 0 through 6");self.read_policy.validate()?;
  ensure!(self.sources.len()<=100,"At most 100 sources are supported");let mut ids=std::collections::HashSet::new();
  for s in &self.sources {ensure!(ids.insert(&s.id),"Duplicate source ID");ensure!(s.access_key_id!="[redacted]" && s.secret_access_key!="[redacted]" && s.session_token.as_deref()!=Some("[redacted]"),"A redacted export cannot be imported; export with includeSecrets=true");}
  if let Some(tls)=&self.tls {tls.validate()?;}
  super::adaptation::validate(self.auto_adaptation_mode.as_deref())?;
  if let Some(drivers)=&self.enabled_drivers {
   ensure!(!drivers.is_empty(),"enabledDrivers must contain at least one driver or be null");
   let unique:std::collections::HashSet<_>=drivers.iter().collect();
   ensure!(unique.len()==drivers.len(),"enabledDrivers contains duplicate driver IDs");
  }
  Ok(())
 }
}
fn path()->Option<PathBuf> {std::env::var_os("RIVER_CONFIGURATION_FILE").map(PathBuf::from).or_else(||std::env::var_os("RIVER_S3_CONNECTIONS_FILE").and_then(|p|PathBuf::from(p).parent().map(|p|p.join("configuration.json")))).or_else(||std::env::var_os("RIVER_ENCODING_SETTINGS_FILE").and_then(|p|PathBuf::from(p).parent().map(|p|p.join("configuration.json"))))}
fn read_file<T:serde::de::DeserializeOwned>(path:&std::path::Path)->Result<Option<T>> {
 let f=match std::fs::File::open(path){Ok(f)=>f,Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e.into())};
 let mut bytes=vec![];f.take(1024*1024+1).read_to_end(&mut bytes)?;ensure!(bytes.len()<=1024*1024,"Configuration exceeds 1 MiB");Ok(Some(serde_json::from_slice(&bytes)?))
}
fn state()->&'static Mutex<Option<Document>> {static STATE:OnceLock<Mutex<Option<Document>>>=OnceLock::new();STATE.get_or_init(||Mutex::new(None))}
pub(super) fn persistence_enabled() -> bool { !env_only() && path().is_some() }

pub(super) fn env_only() -> bool {
    std::env::var("RIVER_CONFIG_MODE").as_deref() == Ok("env")
}

fn apply_environment(mut document: Document) -> Result<Document> {
    fn read(key: &str) -> Result<Option<String>> {
        match std::env::var(key) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => anyhow::bail!("{key} must contain valid text"),
        }
    }

    fn parse<T: std::str::FromStr>(key: &str) -> Result<Option<T>> {
        read(key)?.map(|value| {
            value.parse().map_err(|_| anyhow::anyhow!("Invalid value for {key}"))
        }).transpose()
    }

    fn required(key: &str) -> Result<String> {
        read(key)?.filter(|value| !value.is_empty()).with_context(|| format!("{key} is required"))
    }

    let policy = &mut document.read_policy;
    if let Some(mode) = read("RIVER_S3_ADAPTIVE_READS")? {
        policy.mode = match mode.as_str() {
            "true" | "adaptive" => read_policy::Mode::Adaptive,
            "false" | "disabled" => read_policy::Mode::Disabled,
            "observe" => read_policy::Mode::Observe,
            _ => anyhow::bail!("Invalid value for RIVER_S3_ADAPTIVE_READS"),
        };
    }
    if let Some(transport) = read("RIVER_S3_SERIAL_TRANSPORT")? {
        policy.serial_transport = Some(match transport.as_str() {
            "sdk" => read_policy::SerialTransport::Sdk,
            "curl" => read_policy::SerialTransport::Curl,
            "auto" => read_policy::SerialTransport::Auto,
            _ => anyhow::bail!("Invalid value for RIVER_S3_SERIAL_TRANSPORT"),
        });
    }
    if let Some(refresh) = read("RIVER_S3_REFRESH_PAYLOAD_COSTS")? {
        policy.refresh_payload_costs = Some(match refresh.as_str() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => anyhow::bail!("Invalid value for RIVER_S3_REFRESH_PAYLOAD_COSTS"),
        });
    }
    policy.minimum_samples = parse("RIVER_S3_MINIMUM_SAMPLES")?.unwrap_or(policy.minimum_samples);
    policy.max_overfetch_ratio = parse("RIVER_S3_MAX_OVERFETCH_RATIO")?.unwrap_or(policy.max_overfetch_ratio);
    policy.max_source_requests = parse("RIVER_S3_SOURCE_MAX_REQUESTS")?.or(policy.max_source_requests);
    let mode = read("RIVER_S3_INDEX_HEADERS_MODE")?;
    let window = parse("RIVER_S3_INDEX_HEADERS_WINDOW_BYTES")?;
    let extra = parse("RIVER_S3_INDEX_HEADERS_MAX_EXTRA_BYTES")?;
    if mode.is_some() || window.is_some() || extra.is_some() {
        let existing = policy.index_headers.as_ref();
        let mode = match mode.as_deref() {
            Some("sparse") => read_policy::IndexHeaderMode::Sparse,
            Some("dense") => read_policy::IndexHeaderMode::Dense,
            Some("adaptive") => read_policy::IndexHeaderMode::Adaptive,
            None => existing.map(|index| index.mode).unwrap_or(read_policy::IndexHeaderMode::Adaptive),
            _ => anyhow::bail!("RIVER_S3_INDEX_HEADERS_MODE must be sparse, dense, or adaptive"),
        };
        policy.index_headers = Some(read_policy::IndexHeaders {
            mode,
            window_bytes: window.or_else(|| existing.map(|index| index.window_bytes)).unwrap_or(1024 * 1024),
            max_extra_bytes: extra.or_else(|| existing.map(|index| index.max_extra_bytes)).unwrap_or(32 * 1024 * 1024),
        });
    }

    let count: Option<usize> = parse("RIVER_S3_SOURCE_COUNT")?;
    if let Some(count) = count {
        ensure!(count <= 100, "RIVER_S3_SOURCE_COUNT must be 0 through 100");
        let mut sources = Vec::with_capacity(count);
        for index in 1..=count {
            let key = |field| format!("RIVER_S3_SOURCE_{index}_{field}");
            let id = required(&key("ID"))?;
            sources.push(S3SourceInput {
                name: read(&key("NAME"))?.unwrap_or_else(|| id.clone()),
                id,
                endpoint: read(&key("ENDPOINT"))?,
                bucket: required(&key("BUCKET"))?,
                region: read(&key("REGION"))?.unwrap_or_else(super::default_s3_region),
                prefix: read(&key("PREFIX"))?.unwrap_or_default(),
                access_key_id: required(&key("ACCESS_KEY_ID"))?,
                secret_access_key: required(&key("SECRET_ACCESS_KEY"))?,
                session_token: read(&key("SESSION_TOKEN"))?,
                allow_http: parse(&key("ALLOW_HTTP"))?.unwrap_or(false),
                virtual_hosted_style: parse(&key("VIRTUAL_HOSTED_STYLE"))?.unwrap_or(false),
                max_retries: parse(&key("MAX_RETRIES"))?,
            });
        }
        let mut ids = std::collections::HashSet::new();
        for source in sources {
            ensure!(ids.insert(source.id.clone()), "Duplicate environment source ID");
            document.sources.retain(|existing| existing.id != source.id);
            document.sources.push(source);
        }
    }
    if let Some(drivers) = read("RIVER_ENABLED_DRIVERS")? {
        document.enabled_drivers = Some(drivers.split(',').map(|driver| {
            serde_json::from_value(serde_json::Value::String(driver.trim().to_owned()))
                .context("RIVER_ENABLED_DRIVERS must list valid decoder IDs")
        }).collect::<Result<_>>()?);
    }
    document.encoding.webp_method = parse("RIVER_WEBP_METHOD")?.unwrap_or(document.encoding.webp_method);
    document.validate()?;
    Ok(document)
}

fn defaults() -> Document {
    Document {
        schema_version: 1,
        encoding: encoding_settings::Settings { webp_method: 4 },
        read_policy: read_policy::Settings::default(),
        sources: vec![],
        enabled_drivers: None,
        tls: None,
        logging: None,
        auto_adaptation_mode: None,
        shared_cache: None,
    }
}

fn load() -> Result<Document> {
    let document = match std::env::var("RIVER_CONFIG_MODE").as_deref() {
        Ok("env") => defaults(),
        Ok("json") | Err(std::env::VarError::NotPresent) => load_json()?,
        _ => anyhow::bail!("RIVER_CONFIG_MODE must be env or json"),
    };
    apply_environment(document)
}

fn load_json()->Result<Document> {
 if let Some(p)=path() {if let Some(d)=read_file::<Document>(&p)? {d.validate()?;return Ok(d);}}
 let mut d=defaults();
 if let Some(p)=std::env::var_os("RIVER_S3_CONNECTIONS_FILE").map(PathBuf::from) {
  if let Some(s)=read_file(&p)? {d.sources=s;}
  if let Some(parent)=p.parent() {let encoding=std::env::var_os("RIVER_ENCODING_SETTINGS_FILE").map(PathBuf::from).unwrap_or_else(||parent.join("encoding.json"));if let Some(s)=read_file(&encoding)? {d.encoding=s;}}
 } else if let Some(p)=std::env::var_os("RIVER_ENCODING_SETTINGS_FILE").map(PathBuf::from) {if let Some(s)=read_file(&p)? {d.encoding=s;}}
 d.validate()?;Ok(d)
}
pub fn snapshot()->Result<Document> {let mut s=state().lock().unwrap_or_else(|e|e.into_inner());if s.is_none(){*s=Some(load()?);}Ok(s.as_ref().unwrap().clone())}
pub fn update(change:impl FnOnce(&mut Document)->Result<()>)->Result<Document> {
 let mut state=state().lock().unwrap_or_else(|e|e.into_inner());let before=match state.as_ref(){Some(s)=>s.clone(),None=>load()?};let mut next=before.clone();change(&mut next)?;next.validate()?;
 let Some(p)=path().filter(|_| !env_only()) else {*state=Some(next.clone());return Ok(next);};
 // Persist only API edits. Environment-supplied credentials and overrides
 // must not be copied into the JSON file by unrelated requests.
 let mut stored = serde_json::to_value(load_json()?)?;
 let before = serde_json::to_value(before)?;
 let after = serde_json::to_value(&next)?;
 for key in ["enabledDrivers","tls","logging","autoAdaptationMode","sharedCache"] {
     if before.get(key) == after.get(key) { continue; }
     let disk = stored.as_object_mut().context("Invalid persisted configuration")?;
     if let Some(value) = after.get(key) { disk.insert(key.into(), value.clone()); }
     else { disk.remove(key); }
 }
 for section in ["encoding", "readPolicy"] {
     let old = before[section].as_object().context("Invalid settings section")?;
     let new = after[section].as_object().context("Invalid settings section")?;
     let disk = stored[section].as_object_mut().context("Invalid persisted settings section")?;
     for key in old.keys().chain(new.keys()) {
         if old.get(key) == new.get(key) { continue; }
         if let Some(value) = new.get(key) { disk.insert(key.clone(), value.clone()); }
         else { disk.remove(key); }
     }
 }
 let old_sources = before["sources"].as_array().context("Invalid source list")?;
 let new_sources = after["sources"].as_array().context("Invalid source list")?;
 let disk_sources = stored["sources"].as_array_mut().context("Invalid persisted source list")?;
 for source in old_sources {
     if !new_sources.iter().any(|new| new["id"] == source["id"]) {
         disk_sources.retain(|disk| disk["id"] != source["id"]);
     }
 }
 for source in new_sources {
     if !old_sources.contains(source) {
         disk_sources.retain(|disk| disk["id"] != source["id"]);
         disk_sources.push(source.clone());
     }
 }
 serde_json::from_value::<Document>(stored.clone())?.validate()?;
 let bytes=serde_json::to_vec(&stored)?;ensure!(bytes.len()<=1024*1024,"Configuration exceeds 1 MiB");
 let parent=p.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));std::fs::create_dir_all(parent)?;
 let mut file=tempfile::NamedTempFile::new_in(parent)?;file.write_all(&bytes)?;file.as_file().sync_all()?;file.persist(p)?;*state=Some(next.clone());Ok(next)
}
#[cfg(test)] mod tests {
 use super::*;
 #[test]
 fn environment_configuration_is_fileless() {
     const CHILD: &str = "RIVER_ENV_CONFIG_TEST_CHILD";
     if std::env::var_os(CHILD).is_some() {
         let initial = snapshot().unwrap();
         assert_eq!(initial.encoding.webp_method, 2);
         assert_eq!(initial.read_policy.minimum_samples, 12);
         assert_eq!(initial.sources.len(), 1);
         assert_eq!(initial.sources[0].bucket, "fixture");
         if !env_only() {
             assert_eq!(initial.read_policy.max_overfetch_ratio, 3);
         }
         update(|d| { d.encoding.webp_method = 6; Ok(()) }).unwrap();
         assert_eq!(snapshot().unwrap().encoding.webp_method, 6);
         assert_eq!(load().unwrap().encoding.webp_method, 2);
         return;
     }

     let directory = tempfile::tempdir().unwrap();
     let json = directory.path().join("configuration.json");
     std::fs::write(&json, b"invalid JSON").unwrap();
     let mut command = std::process::Command::new(std::env::current_exe().unwrap());
     for (key, _) in std::env::vars_os() {
         if key.to_string_lossy().starts_with("RIVER_") {
             command.env_remove(key);
         }
     }
     let result = command
         .args(["--exact", "configuration::tests::environment_configuration_is_fileless"])
         .env(CHILD, "1")
         .env("RIVER_CONFIG_MODE", "env")
         .env("RIVER_CONFIGURATION_FILE", &json)
         .env("RIVER_WEBP_METHOD", "2")
         .env("RIVER_S3_MINIMUM_SAMPLES", "12")
         .env("RIVER_S3_SOURCE_COUNT", "1")
         .env("RIVER_S3_SOURCE_1_ID", "example")
         .env("RIVER_S3_SOURCE_1_BUCKET", "fixture")
         .env("RIVER_S3_SOURCE_1_ACCESS_KEY_ID", "fixture")
         .env("RIVER_S3_SOURCE_1_SECRET_ACCESS_KEY", "fixture")
         .output().unwrap();
     assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
     assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
     assert_eq!(std::fs::read(&json).unwrap(), b"invalid JSON");
     let mut persisted = document();
     persisted["readPolicy"]["maxOverfetchRatio"] = 3.into();
     std::fs::write(&json, serde_json::to_vec(&persisted).unwrap()).unwrap();
     let result = command.env("RIVER_CONFIG_MODE", "json").output().unwrap();
     assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stdout));
     assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
     let persisted: Document = read_file(&json).unwrap().unwrap();
     assert_eq!(persisted.encoding.webp_method, 6);
     assert_eq!(persisted.read_policy.minimum_samples, 16);
     assert!(persisted.sources.is_empty());
 }
 fn document()->serde_json::Value {serde_json::json!({"schemaVersion":1,"encoding":{"webpMethod":0},"readPolicy":{"mode":"observe","minimumSamples":16,"maxOverfetchRatio":2},"sources":[]})}
 #[test] fn versioned_document_rejects_unknown_fields_and_future_versions() {let mut d=document();d["schemaVersion"]=2.into();assert!(serde_json::from_value::<Document>(d.clone()).unwrap().validate().is_err());d["schemaVersion"]=1.into();d["typo"]=true.into();assert!(serde_json::from_value::<Document>(d).is_err());}
 #[test] fn redacted_exports_and_duplicate_sources_cannot_replace_configuration() {let mut d=document();let s=serde_json::json!({"id":"test","name":"test","bucket":"test","accessKeyId":"key","secretAccessKey":"[redacted]"});d["sources"]=serde_json::json!([s.clone()]);assert!(serde_json::from_value::<Document>(d.clone()).unwrap().validate().is_err());d["sources"]=serde_json::json!([s.clone(),s]);d["sources"][0]["secretAccessKey"]="secret".into();d["sources"][1]["secretAccessKey"]="secret".into();assert!(serde_json::from_value::<Document>(d).unwrap().validate().is_err());}
}
