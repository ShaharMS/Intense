use super::*;
pub async fn export(State(state):State<AppState>)->Response {
 let _guard=state.configuration_lock.lock().await;
 let mut value=match configuration::snapshot(){Ok(d)=>serde_json::to_value(d).unwrap(),Err(_)=>return persistence_error()};
 // Include in-memory sources too when persistence is intentionally unconfigured.
 let mut sources=state.s3_sources.read().await.values().map(|s|s.connection.clone()).collect::<Vec<_>>();sources.sort_by(|a,b|a.id.cmp(&b.id));value["sources"]=serde_json::to_value(sources).unwrap();
 value["readPolicy"]=read_policy::settings_view(&read_policy::settings());
 ([(axum::http::header::CACHE_CONTROL,"no-store")],Json(value)).into_response()
}
fn persistence_error()->Response {errors::reply(StatusCode::INTERNAL_SERVER_ERROR,"configuration_persistence_failed","Could not persist configuration. Check the configured file path, write access and free space; active settings were not replaced.",serde_json::json!({}))}
pub async fn import(State(state):State<AppState>,Json(document):Json<configuration::Document>)->Response {
 if let Err(e)=document.validate(){return errors::reply(StatusCode::BAD_REQUEST,"invalid_configuration",e.to_string(),serde_json::json!({}));}
 let mut replacement=HashMap::new();
 for input in &document.sources {match source_from_input(input){Ok(s)=>{replacement.insert(s.id.clone(),s);},Err(_)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_s3_source","An imported S3 connection is invalid; no configuration was changed.",serde_json::json!({"sourceId":input.id}))}}
 let _guard=state.configuration_lock.lock().await;let _load=state.s3_load_lock.write().await;let mut sources=state.s3_sources.write().await;
 if configuration::update(|d|{*d=document.clone();Ok(())}).is_err(){return persistence_error();}
 encoding_settings::apply(document.encoding);read_policy::apply(document.read_policy);read_policy::reset_all();*sources=replacement;drop(sources);
 state.assets.write().await.retain(|id,asset|remote_asset_key(&id.0).is_none() && asset.source.as_ref().and_then(|s|s.remote.as_ref()).is_none_or(|r|r.local_path.is_some()));
 Json(serde_json::json!({"schemaVersion":1,"applied":true,"persisted":true,"sources":document.sources.len(),"credentialsReturned":false,"measurementsReset":true})).into_response()
}
pub async fn get_policy()->Response {
 ([(axum::http::header::CACHE_CONTROL,"no-store")],Json(serde_json::json!({"settings":read_policy::settings_view(&read_policy::settings()),"sources":read_policy::observations(),"explicitOverfetchCap":read_policy::explicit_overfetch_cap(),"effectiveAdaptiveOverfetchRatio":u64::from(read_policy::settings().max_overfetch_ratio).min(read_policy::explicit_overfetch_cap().unwrap_or(16)),"learning":{"throughputSampleMinimumBytes":read_policy::THROUGHPUT_SAMPLE_BYTES,"minimumThroughputSamples":3,"indexDiscoveryCalibrationReads":crate::remote_source::INDEX_CALIBRATION_READS,"maximumPlannerRanges":16384,"maximumMergedPrefixRangeBytes":crate::remote_source::PREFIX_MERGE_BYTES,"prefixTransferEstimateBudgetMs":8000,"rangeNetworkTimeoutMs":10000,"rangeNetworkTimeoutScope":"serialTotalAndBatchedResponseHeaders","batchedSdkBodyIdleTimeoutMs":10000,"preparationDeadlineMs":30000,"compatibilityTransferDeadlineMs":300000,"maximumRequiredContiguousRangeBytes":read_policy::MAX_CONTIGUOUS_RANGE_BYTES,"contiguousBodyEstimateBudgetMs":read_policy::CONTIGUOUS_BODY_BUDGET_MS,"indexHeaderWindowMaximumBytes":crate::remote_source::HEADER_WINDOW_MAX_BYTES,"indexHeaderRetainedBlockMaximumBytes":crate::remote_source::HEADER_WINDOW_RETAINED_BYTES,"rangeKinds":["packetBatches","multipartResolutionPrefixes","indexOnlyHeaderWindows"]},"measurementScope":"activeReads counts admitted range operations including retries/backoff; admissionWaits counts operations queued at least once. Samples and bytes count successfully fetched ranges, not physical retry attempts. effectiveBytesPerSecond retains whole-request throughput. bodyBytesPerSecond excludes measured header latency; planners use it after three bodyThroughputSamples so RTT is not charged twice. Missing native bulk header timing retains the whole-request fallback. Serial transport counters exclude initial probes and batched SDK payload reads. Measurements are process-local and reset on restart or connection replacement. contiguousRanges records the last zero-gap assembly decision, its sample counts and size cap. Its cap uses the lower of the body EWMA and lastBodyBytesPerSecond after the configured minimumSamples and three body samples. Batched SDK transfers have a response-header timeout and a body-progress idle timeout, plus their operation deadline; active data may exceed one idle interval. No synthetic probes."}))).into_response()
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
