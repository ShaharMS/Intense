//! One error contract, including extractor, static-service and router rejections.
use axum::{body::{Body,to_bytes},extract::Request,http::{StatusCode,header},middleware::Next,response::{Response,IntoResponse},Json};
use serde_json::{Value,json};

pub fn reply(status:StatusCode,code:&str,message:impl Into<String>,details:Value)->Response {
    (status,Json(json!({"code":code,"message":message.into(),"details":details}))).into_response()
}

fn explanation(code:&str,status:StatusCode)->&str {
    match code {
        "unauthorized"=>"A valid Authorization: Bearer <RIVER_ADMIN_TOKEN> header is required for this operation.",
        "invalid_s3_source"=>"Invalid S3 connection. Supply a valid id, bucket, region, endpoint and credential configuration.",
        "s3_source_not_found"=>"The requested S3 connection does not exist. List /api/s3/sources and select a connected source ID.",
        "metadata_mismatch"=>"Supplied width, height or bitDepth does not match the JPEG 2000 header. Omit these hints or use the actual image dimensions.",
        "object_outside_source_prefix"=>"The object key is outside the configured S3 source prefix. Use a key within that prefix.",
        "invalid_search_cursor"=>"The search cursor is invalid or belongs to a different search. Restart the search without a cursor.",
        "source_prepare_worker_failed"=>"The source-preparation worker stopped unexpectedly. Check the server logs using the request ID and retry.",
        "decode_worker_failed"=>"The decode worker terminated before returning an image. Check the server logs for the worker failure and retry.",
        "encoding_settings_persistence_failed"=>"Could not persist encoding settings. Check write access and free space for the River configuration volume.",
        "asset_not_found"=>"The requested image is unavailable. List /api/assets or use a connected S3 locator.",
        _=>match status.as_u16() {
            400|422=>"The request contains invalid parameters. Check the endpoint schema in /docs.",
            401=>"Authentication is required for this operation.",
            404=>"No endpoint or static resource exists at this path. Check the URL and /docs.",
            405=>"This HTTP method is not supported at this path. Check the Allow header or /docs.",
            413=>"The request body exceeds the allowed size. Send a smaller request.",
            415=>"Unsupported request content type. JSON endpoints require Content-Type: application/json.",
            503=>"The requested service is unavailable. Check runtime driver availability and server readiness.",
            _=>"The operation failed. Use the request ID to locate the server diagnostic.",
        }
    }
}

pub async fn normalize(request:Request,next:Next)->Response {
    let path=request.uri().path().to_owned(); // Never echo query strings, bearer tokens or signed URLs.
    let method=request.method().to_string();
    let request_id=uuid::Uuid::new_v4().to_string();
    let response=next.run(request).await;
    if !response.status().is_client_error() && !response.status().is_server_error() {return response;}
    let (mut parts,body)=response.into_parts();
    let bytes=to_bytes(body,1024*1024).await.unwrap_or_default();
    let mut payload=serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object).unwrap_or_else(||json!({}));
    let default_code=match parts.status.as_u16() {400=>"invalid_request",404=>"route_not_found",405=>"method_not_allowed",413=>"request_too_large",415=>"unsupported_content_type",422=>"invalid_request_body",_=>"request_failed"};
    let code=payload.get("code").and_then(Value::as_str).unwrap_or(default_code).to_owned();
    let message=payload.get("message").and_then(Value::as_str).filter(|v|!v.trim().is_empty()).map(str::to_owned)
        .or_else(||std::str::from_utf8(&bytes).ok().filter(|text|!text.trim().is_empty() && !text.trim_start().starts_with('{')).map(str::to_owned))
        .unwrap_or_else(||explanation(&code,parts.status).to_owned());
    payload["code"]=json!(code);payload["message"]=json!(message);
    payload["status"]=json!(parts.status.as_u16());payload["path"]=json!(path);payload["method"]=json!(method);payload["requestId"]=json!(request_id);
    if payload.get("details").is_none() {payload["details"]=json!({});}
    tracing::warn!(%request_id,status=parts.status.as_u16(),code=%code,path=%path,method=%method,"HTTP request failed");
    parts.headers.remove(header::CONTENT_LENGTH);parts.headers.remove(header::CONTENT_ENCODING);
    parts.headers.insert(header::CONTENT_TYPE,header::HeaderValue::from_static("application/json"));
    parts.headers.insert(header::CACHE_CONTROL,header::HeaderValue::from_static("no-store"));
    parts.headers.insert("x-river-request-id",header::HeaderValue::from_str(&request_id).unwrap());
    Response::from_parts(parts,Body::from(serde_json::to_vec(&payload).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn every_known_code_has_actionable_message() {
        for code in ["unauthorized","invalid_s3_source","s3_source_not_found","metadata_mismatch","object_outside_source_prefix","invalid_search_cursor","decode_worker_failed","source_prepare_worker_failed","encoding_settings_persistence_failed","asset_not_found"] {
            assert!(explanation(code,StatusCode::BAD_REQUEST).len()>40);
        }
    }
}
