use axum::{http::header, response::{Html, IntoResponse}};
pub async fn spec() -> impl IntoResponse {
    static SPEC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let spec=SPEC.get_or_init(|| {
        let mut value: serde_json::Value=serde_json::from_str(include_str!("../../../../deploy/openapi.json")).expect("embedded OpenAPI is valid");
        if let Some(version)=option_env!("RIVER_BUILD_VERSION") {value["info"]["version"]=version.into();}
        serde_json::to_string(&value).expect("OpenAPI serializes")
    });
    ([(header::CONTENT_TYPE, "application/json")], spec.as_str())
}
pub async fn ui() -> Html<&'static str> { Html(include_str!("../../../../deploy/swagger/index.html")) }

#[cfg(test)]
mod tests {
    #[test]
    fn specification_covers_public_routes() {
        let spec: serde_json::Value = serde_json::from_str(include_str!("../../../../deploy/openapi.json")).unwrap();
        for route in ["/ws", "/ws/tiles", "/api/ws-capabilities"] {
            assert!(spec["paths"].get(route).is_none(), "retired route {route}");
        }
        for route in ["/api/tiles/{image}", "/health", "/api/assets/{asset_id}/manifest", "/api/drivers", "/api/settings/cache-learning", "/api/settings/shared-cache", "/api/settings/adaptation", "/api/formats", "/api/cache/ht", "/wmts", "/api/s3/sources", "/api/s3/sources/{source_id}", ] {
            assert!(spec["paths"].get(route).is_some(), "undocumented route {route}");
        }
    }
}
