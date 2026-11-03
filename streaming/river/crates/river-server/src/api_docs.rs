use axum::{http::header, response::{Html, IntoResponse}};
pub async fn spec() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/json")], include_str!("../../../deploy/openapi.json"))
}
pub async fn ui() -> Html<&'static str> { Html(include_str!("../../../deploy/swagger/index.html")) }

#[cfg(test)]
mod tests {
    #[test]
    fn specification_covers_public_routes() {
        let spec: serde_json::Value = serde_json::from_str(include_str!("../../../deploy/openapi.json")).unwrap();
        for route in ["/api/tiles/{image}", "/health", "/api/assets", "/api/assets/{asset_id}/manifest", "/api/drivers", "/api/formats", "/api/cache/ht", "/wmts", "/ws", "/ws/tiles", "/api/s3/sources", "/api/s3/sources/{source_id}", "/api/s3/sources/{source_id}/objects", "/api/s3/sources/{source_id}/assets"] {
            assert!(spec["paths"].get(route).is_some(), "undocumented route {route}");
        }
    }
}
