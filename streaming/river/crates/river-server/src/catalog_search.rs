//! Matching-result pages, with bounded provider work and resumable scans.
use super::*;

#[derive(Default,serde::Serialize,serde::Deserialize)]
struct Cursor { signature:String, source:usize, token:Option<String> }

pub(super) async fn search(state:AppState,query:AssetListQuery)->Response {
    let scope=query.source.as_deref().unwrap_or("all");
    let mut sources=state.s3_sources.read().await.values()
        .filter(|source|scope=="all" || source.id==scope).cloned().collect::<Vec<_>>();
    sources.sort_by(|a,b|a.id.cmp(&b.id));
    if scope!="all" && sources.is_empty() {
        return (StatusCode::NOT_FOUND,Json(serde_json::json!({"code":"s3_source_not_found"}))).into_response();
    }
    let search=query.q.unwrap_or_default().trim().to_lowercase();
    let prefix=query.key_prefix.unwrap_or_default();
    let signature=hex::encode(Sha256::digest(serde_json::to_vec(&(scope,&search,&prefix,
        sources.iter().map(|s|(&s.id,&s.prefix,&s.bucket,&s.endpoint)).collect::<Vec<_>>())).unwrap()));
    let mut cursor=match query.cursor {
        Some(value)=>match decode_cursor(&value,&signature) {
            Some(cursor)=>cursor,None=>return (StatusCode::BAD_REQUEST,Json(serde_json::json!({"code":"invalid_search_cursor"}))).into_response(),
        },
        None=>Cursor {signature,..Default::default()},
    };
    let limit=page_limit(query.limit);
    let mut objects=Vec::new();let mut scanned=0;let mut calls=0;
    let started=Instant::now();
    while cursor.source<sources.len() && objects.len()<limit && calls<10 && started.elapsed()<Duration::from_secs(2) {
        let source=&sources[cursor.source];
        let base=source.prefix.trim_matches('/');
        let prefix=format!("{}{}{}",base,if base.is_empty() {""} else {"/"},prefix.trim_start_matches('/'));
        let page=match source.store.list_paginated((!prefix.is_empty()).then_some(prefix.as_str()),PaginatedListOptions {
            max_keys:Some(limit-objects.len()),page_token:cursor.token.clone(),..Default::default()
        }).await {
            Ok(page)=>page,
            Err(error)=>return (StatusCode::BAD_GATEWAY,Json(serde_json::json!({"code":"s3_list_failed","sourceId":source.id,"message":error.to_string()}))).into_response(),
        };
        calls+=1;scanned+=page.result.objects.len();
        objects.extend(page.result.objects.iter().filter(|meta|is_jpeg2000_key(meta.location.as_ref())
            && (search.is_empty() || meta.location.as_ref().to_lowercase().contains(&search))).map(|meta|serde_json::json!({
                "key":meta.location.as_ref(),"name":meta.location.as_ref(),"sourceId":source.id,
                "assetId":remote_asset_id(&source.id,meta.location.as_ref()),"size":meta.size,"lastModified":meta.last_modified.to_string()
            })));
        cursor.token=page.page_token;
        if cursor.token.is_none() {cursor.source+=1;}
    }
    let next=(cursor.source<sources.len()).then(||hex::encode(serde_json::to_vec(&cursor).unwrap()));
    Json(serde_json::json!({"objects":objects,"scannedObjects":scanned,"providerRequests":calls,
        "truncated":next.is_some(),"nextCursor":next})).into_response()
}

fn decode_cursor(value:&str,signature:&str)->Option<Cursor> {
    if value.len()>32768 {return None;}
    let cursor:Cursor=serde_json::from_slice(&hex::decode(value).ok()?).ok()?;
    (cursor.signature==signature).then_some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuation_is_query_bound_and_preserves_provider_position() {
        let value=hex::encode(serde_json::to_vec(&Cursor {signature:"query".into(),source:2,token:Some("provider-token".into())}).unwrap());
        let decoded=decode_cursor(&value,"query").unwrap();
        assert_eq!(decoded.source,2);assert_eq!(decoded.token.as_deref(),Some("provider-token"));
        assert!(decode_cursor(&value,"different-query").is_none());
        assert!(decode_cursor("invalid","query").is_none());
        assert!(decode_cursor(&"0".repeat(32769),"query").is_none());
    }
}
