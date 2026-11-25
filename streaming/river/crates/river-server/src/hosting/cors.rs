//! One browser-access policy shared by every route and release channel.
use anyhow::{bail, Context, Result};
use axum::{extract::Request, http::{header, HeaderValue}, middleware::Next, response::Response};
use std::time::Duration;
use tower_http::cors::CorsLayer;

pub fn layer(origins: &str) -> Result<CorsLayer> {
    let origins = origins.trim();
    // Reflect rather than emit '*' so credentialed browser requests also work.
    let mut layer = CorsLayer::very_permissive()
        .allow_private_network(true)
        .max_age(Duration::from_secs(600))
        .vary([
            header::ORIGIN,
            header::ACCESS_CONTROL_REQUEST_METHOD,
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            axum::http::HeaderName::from_static("access-control-request-private-network"),
        ]);
    if !origins.is_empty() && origins != "*" {
        let mut allowed = Vec::new();
        for origin in origins.split(',').map(str::trim) {
            if origin.is_empty() || origin == "*" {
                bail!("RIVER_CORS_ORIGINS must be '*' or a comma-separated list of complete origins");
            }
            if origin != "null" {
                let uri: axum::http::Uri = origin.parse().context("Invalid RIVER_CORS_ORIGINS origin")?;
                if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none()
                    || uri.authority().is_some_and(|a| a.as_str().contains('@'))
                    || uri.path_and_query().is_some_and(|p| p.as_str() != "/") {
                    bail!("RIVER_CORS_ORIGINS entries must be origins, without paths or query strings: {origin}");
                }
            }
            // Origin values do not contain a trailing slash.
            allowed.push(origin.trim_end_matches('/').parse::<HeaderValue>()
                .context("Invalid RIVER_CORS_ORIGINS header")?);
        }
        layer = layer.allow_origin(allowed);
    }
    Ok(layer)
}

// Explicit names work with credentials; Expose-Headers: * does not. Derive them
// from each response so new raster/diagnostic headers cannot silently disappear.
pub async fn expose_response_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let names = response.headers().keys()
        .filter(|name| !matches!(name.as_str(), "set-cookie" | "set-cookie2" | "access-control-expose-headers"))
        .map(|name| name.as_str()).collect::<Vec<_>>().join(", ");
    if !names.is_empty() {
        response.headers_mut().insert(header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_str(&names).expect("HeaderMap keys are valid header names"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn hosting_defaults_and_explicit_origins_parse() {
        for origins in ["*", "", "https://crystal.example", "http://localhost:19074, https://viewer.example/", "null"] {
            assert!(layer(origins).is_ok(), "{origins}");
        }
    }
    #[test] fn malformed_restrictions_fail_closed() {
        for origins in ["*,https://example.com", "https://example.com,", "example.com", "https://example.com/path", "https://example.com/?q=x", "https://user@example.com"] {
            assert!(layer(origins).is_err(), "{origins}");
        }
    }
}
