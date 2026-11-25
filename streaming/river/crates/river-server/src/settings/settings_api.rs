use super::*;
pub async fn export(State(state):State<AppState>)->Response {
 let _guard=state.configuration_lock.lock().await;
 let mut value=match configuration::snapshot(){Ok(d)=>serde_json::to_value(d).unwrap(),Err(_)=>return persistence_error()};
 // The cache URL can contain credentials. Inspect its non-secret settings
 // through the shared-cache endpoint.
 if let Some(fields) = value.as_object_mut() { fields.remove("sharedCache"); }
 // Include in-memory sources too when persistence is intentionally unconfigured.
 let mut sources=state.s3_sources.read().await.values().map(|s|s.connection.clone()).collect::<Vec<_>>();sources.sort_by(|a,b|a.id.cmp(&b.id));value["sources"]=serde_json::to_value(sources).unwrap();
 value["readPolicy"]=read_policy::settings_view(&read_policy::settings());
 ([(axum::http::header::CACHE_CONTROL,"no-store")],Json(value)).into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdaptationChange { mode: String }

pub async fn get_adaptation() -> Json<serde_json::Value> { Json(adaptation::snapshot()) }

fn require_admin(headers: &axum::http::HeaderMap) -> Option<Response> {
    let allowed = std::env::var("RIVER_ADMIN_TOKEN").ok().filter(|value| !value.is_empty())
        .zip(headers.get(axum::http::header::AUTHORIZATION).and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer ")))
        .is_some_and(|(expected, provided)| {
            let a = Sha256::digest(expected.as_bytes());
            let b = Sha256::digest(provided.as_bytes());
            a.iter().zip(b).fold(0u8, |difference, (x, y)| difference | (x ^ y)) == 0
        });
    (!allowed).then(|| errors::reply(StatusCode::UNAUTHORIZED, "unauthorized",
        "Configure RIVER_ADMIN_TOKEN and supply it as a bearer token to change live operational settings.",
        serde_json::json!({})))
}

pub async fn put_adaptation(headers: axum::http::HeaderMap, Json(change): Json<AdaptationChange>) -> Response {
    if let Some(response) = require_admin(&headers) { return response; }
    if let Err(error) = adaptation::set_live(Some(&change.mode)) {
        return errors::reply(StatusCode::BAD_REQUEST, "invalid_adaptation_mode", error.to_string(), serde_json::json!({}));
    }
    tokio::task::spawn_blocking(resources::reclaim);
    get_adaptation().await.into_response()
}

pub async fn reset_adaptation(headers: axum::http::HeaderMap) -> Response {
    if let Some(response) = require_admin(&headers) { return response; }
    if adaptation::set_live(None).is_err() { return persistence_error(); }
    tokio::task::spawn_blocking(resources::reclaim);
    get_adaptation().await.into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedCacheChange { settings: Option<shared_cache::Settings> }

pub async fn get_shared_cache() -> Json<serde_json::Value> { Json(shared_cache::diagnostics()) }

pub async fn put_shared_cache(headers: axum::http::HeaderMap, Json(change): Json<SharedCacheChange>) -> Response {
    if let Some(response) = require_admin(&headers) { return response; }
    if let Err(error) = shared_cache::replace(change.settings).await {
        return errors::reply(StatusCode::BAD_REQUEST, "shared_cache_connection_failed", error.to_string(), serde_json::json!({}));
    }
    get_shared_cache().await.into_response()
}

pub async fn reset_shared_cache(headers: axum::http::HeaderMap) -> Response {
    if let Some(response) = require_admin(&headers) { return response; }
    if let Err(error) = shared_cache::reset().await {
        return errors::reply(StatusCode::BAD_GATEWAY, "shared_cache_connection_failed", error.to_string(), serde_json::json!({}));
    }
    get_shared_cache().await.into_response()
}
fn persistence_error()->Response {errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"configuration_persistence_failed","Could not persist configuration. Check the configured file path, write access and free space; active settings were not replaced.",serde_json::json!({}))}
pub async fn import(State(state):State<AppState>,Json(mut document):Json<configuration::Document>)->Response {
 if let Err(e)=document.validate(){return errors::reply(StatusCode::BAD_REQUEST,"invalid_configuration",e.to_string(),serde_json::json!({}));}
 let mut replacement=HashMap::new();
 for input in &document.sources {match source_from_input(input){Ok(s)=>{replacement.insert(s.id.clone(),s);},Err(_)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_s3_source","An imported S3 connection is invalid; no configuration was changed.",serde_json::json!({"sourceId":input.id}))}}
 let _guard=state.configuration_lock.lock().await;let _load=state.s3_load_lock.write().await;let mut sources=state.s3_sources.write().await;
 let Ok(existing) = configuration::snapshot() else { return persistence_error(); };
 document.shared_cache = existing.shared_cache;
 document.auto_adaptation_mode = existing.auto_adaptation_mode;
 if document.enabled_drivers.is_none() {
  document.enabled_drivers=match configuration::snapshot(){Ok(d)=>d.enabled_drivers,Err(_)=>return persistence_error()};
 }
 if let Err(error)=state.tile_engines.validate_enabled(document.enabled_drivers.as_deref()) {
  return driver_error(error);
 }
 if configuration::update(|d|{*d=document.clone();Ok(())}).is_err(){return persistence_error();}
 if let Some(enabled)=document.cache_learning {cache_learning::set(enabled);}
 state.tile_engines.apply_enabled(document.enabled_drivers.clone());
 encoding_settings::apply(document.encoding);read_policy::apply(document.read_policy);read_policy::reset_all();*sources=replacement;drop(sources);
 state.assets.write().await.retain(|id,asset|remote_asset_key(&id.0).is_none() && asset.source.as_ref().and_then(|s|s.remote.as_ref()).is_none_or(|r|r.local_path.is_some()));
 Json(serde_json::json!({"schemaVersion":1,"applied":true,"persisted":configuration::persistence_enabled(),"sources":document.sources.len(),"credentialsReturned":false,"measurementsReset":true})).into_response()
}
pub async fn get_policy()->Response {
 let configured = read_policy::settings();
 let mut sources=serde_json::to_value(read_policy::observations()).unwrap();
 if let Some(sources)=sources.as_object_mut() {
  for source in sources.values_mut() {
   for (key,scope) in [("actualOverfetch","render preparation"),("metadataOverfetch","initial metadata discovery")] {
    if let Some(feedback)=source.get_mut(key).and_then(|v|v.as_object_mut()) {
     let bytes=feedback.get("measuredOriginBytes").and_then(|v|v.as_u64()).unwrap_or(0);
     let needed=feedback.get("neededOriginBytes").and_then(|v|v.as_u64()).unwrap_or(0);
     feedback.insert("measuredRatio".into(),if needed>0 {serde_json::json!(bytes as f64/needed as f64)}else{serde_json::Value::Null});
     feedback.insert("scope".into(),scope.into());
     feedback.insert("measurementDefinition".into(),"Successful origin response bytes / unique consumer-demanded bytes intersecting those responses; block padding and gap filling count as extra. Cache-only demand is excluded. Failed/partial/retry bodies are not observable here; incomplete samples do not train. This is not a theoretical minimum codestream size.".into());
     let ceiling=u64::from(configured.max_overfetch_ratio).min(read_policy::explicit_overfetch_cap().unwrap_or(16))*1000;
     let applied=key=="actualOverfetch" && configured.mode==read_policy::Mode::Adaptive;
     let effective=if applied {feedback.get("learnedRatioMilli").and_then(|v|v.as_u64()).unwrap_or(ceiling).clamp(1000,ceiling)}else{ceiling};
     feedback.insert("feedbackApplied".into(),applied.into());
     feedback.insert("effectiveOptionalReadRatio".into(),serde_json::json!(effective as f64/1000.0));
    }
   }

   for key in ["lastDecision","indexHeaderDecision","contiguousRanges"] {
    if let Some(decision)=source.get_mut(key).and_then(|v|v.as_object_mut()) {
     if let Some(reason)=decision.get("reason").and_then(|v|v.as_str()) {
      let mut text=String::new();for c in reason.chars(){if c.is_ascii_uppercase()&&!text.is_empty(){text.push(' ');}text.push(c.to_ascii_lowercase());}
      decision.insert("reasonText".into(),text.into());
     }
     if let (Some(original),Some(planned))=(decision.get("originalBytes").and_then(|v|v.as_u64()),decision.get("plannedBytes").and_then(|v|v.as_u64())) {
      if original>0 {decision.insert("plannedOverfetchRatio".into(),serde_json::json!(planned as f64/original as f64));}
      decision.insert("ratioScope".into(),"Planned range bytes divided by required range bytes; not actual wire bytes or measured latency savings".into());
     }
    }
   }
  }
 }
 ([(axum::http::header::CACHE_CONTROL,"no-store")],Json(serde_json::json!({"settings":read_policy::settings_view(&configured),"configuredSettings":&configured,"sources":sources,"explicitOverfetchCap":read_policy::explicit_overfetch_cap(),"effectiveAdaptiveOverfetchRatio":u64::from(configured.max_overfetch_ratio).min(read_policy::explicit_overfetch_cap().unwrap_or(16)),"learning":{"throughputSampleMinimumBytes":read_policy::THROUGHPUT_SAMPLE_BYTES,"minimumThroughputSamples":3,"indexDiscoveryCalibrationReads":crate::remote_source::INDEX_CALIBRATION_READS,"maximumPlannerRanges":16384,"maximumMergedPrefixRangeBytes":crate::remote_source::PREFIX_MERGE_BYTES,"prefixTransferEstimateBudgetMs":8000,"rangeNetworkTimeoutMs":10000,"rangeNetworkTimeoutScope":"serialTotalAndBatchedResponseHeaders","batchedSdkBodyIdleTimeoutMs":10000,"preparationDeadlineMs":30000,"compatibilityTransferDeadlineMs":300000,"maximumRequiredContiguousRangeBytes":read_policy::MAX_CONTIGUOUS_RANGE_BYTES,"contiguousBodyEstimateBudgetMs":read_policy::CONTIGUOUS_BODY_BUDGET_MS,"indexHeaderWindowMaximumBytes":crate::remote_source::HEADER_WINDOW_MAX_BYTES,"indexHeaderRetainedBlockMaximumBytes":crate::remote_source::HEADER_WINDOW_RETAINED_BYTES,"rangeKinds":["packetBatches","multipartResolutionPrefixes","indexOnlyHeaderWindows"]},"measurementScope":"activeReads counts admitted range operations including retries/backoff; admissionWaits counts operations queued at least once. Samples and bytes count successfully fetched ranges, not physical retry attempts. effectiveBytesPerSecond retains whole-request throughput. bodyBytesPerSecond excludes measured header latency; planners use it after three bodyThroughputSamples so RTT is not charged twice. Missing native bulk header timing retains the whole-request fallback. Serial transport counters exclude initial probes and batched SDK payload reads. Measurements are process-local and reset on restart or connection replacement. contiguousRanges records the last zero-gap assembly decision, its sample counts and size cap. Its cap uses the lower of the body EWMA and lastBodyBytesPerSecond after the configured minimumSamples and three body samples. Batched SDK transfers have a response-header timeout and a body-progress idle timeout, plus their operation deadline; active data may exceed one idle interval. No synthetic probes."}))).into_response()
}
pub async fn put_policy(State(state):State<AppState>,Json(settings):Json<read_policy::Settings>)->Response {
 if let Err(e)=settings.validate(){return errors::reply(StatusCode::BAD_REQUEST,"invalid_read_policy",e.to_string(),serde_json::json!({}));}
 let _guard=state.configuration_lock.lock().await;
 if configuration::update(|d|{d.read_policy=settings.clone();Ok(())}).is_err(){return persistence_error();}
 read_policy::apply(settings.clone());Json(read_policy::settings_view(&settings)).into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReadTraceSettings {enabled:bool}
pub async fn get_source_reads()->Response {
 ([(axum::http::header::CACHE_CONTROL,"no-store")],Json(remote_source::read_trace::snapshot())).into_response()
}
pub async fn put_source_reads(Json(settings):Json<SourceReadTraceSettings>)->Response {
 remote_source::read_trace::configure(settings.enabled);get_source_reads().await
}
pub async fn clear_source_reads()->Response {
 remote_source::read_trace::clear();get_source_reads().await
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DriverSettings {
    enabled_drivers: Option<Vec<DecoderDriver>>,
}

fn driver_error(error: anyhow::Error) -> Response {
    errors::reply(StatusCode::BAD_REQUEST, "invalid_enabled_drivers", error.to_string(), serde_json::json!({}))
}

pub async fn get_drivers(State(state): State<AppState>) -> Response {
    let _guard = state.configuration_lock.lock().await;
    match configuration::snapshot() {
        Ok(document) => Json(DriverSettings { enabled_drivers: document.enabled_drivers }).into_response(),
        Err(_) => persistence_error(),
    }
}

pub async fn put_drivers(State(state): State<AppState>, Json(settings): Json<DriverSettings>) -> Response {
    let _guard = state.configuration_lock.lock().await;
    if let Err(error) = state.tile_engines.validate_enabled(settings.enabled_drivers.as_deref()) {
        return driver_error(error);
    }
    match configuration::snapshot().and_then(|mut document| {
        document.enabled_drivers = settings.enabled_drivers.clone();
        document.validate()
    }) {
        Ok(()) => {},
        Err(error) => return driver_error(error),
    }
    if configuration::update(|document| {
        document.enabled_drivers = settings.enabled_drivers.clone();
        Ok(())
    }).is_err() {
        return persistence_error();
    }
    state.tile_engines.apply_enabled(settings.enabled_drivers.clone());
    Json(settings).into_response()
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheLearningChange { enabled: bool }
pub async fn get_cache_learning() -> Json<serde_json::Value> {Json(cache_learning::diagnostics())}
pub async fn put_cache_learning(headers: axum::http::HeaderMap, Json(change): Json<CacheLearningChange>) -> Response {
    if let Some(response)=require_admin(&headers) {return response;}
    cache_learning::set(change.enabled);
    tokio::task::spawn_blocking(resources::reclaim);
    get_cache_learning().await.into_response()
}
pub async fn reset_cache_learning(headers: axum::http::HeaderMap) -> Response {
    if let Some(response)=require_admin(&headers) {return response;}
    cache_learning::reset();tokio::task::spawn_blocking(resources::reclaim);
    get_cache_learning().await.into_response()
}
