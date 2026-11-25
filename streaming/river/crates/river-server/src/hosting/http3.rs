use std::{net::SocketAddr, sync::Arc};
use anyhow::{Context, Result};
use axum::{Router, body::{Body, Bytes}, http::{Method, Response, StatusCode, Version}};
use axum_server::tls_rustls::RustlsConfig;
use bytes::Buf;
use futures_util::{StreamExt, stream::try_unfold};
use tower::ServiceExt;

pub fn bind(address: SocketAddr, identity: &RustlsConfig) -> Result<quinn::Endpoint> {
    let mut tls = (*identity.get_inner()).clone();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(64u32.into());
    config.transport_config(Arc::new(transport));
    quinn::Endpoint::server(config, address)
        .with_context(|| format!("Could not bind River HTTP/3 on UDP {address}"))
}

pub async fn serve(endpoint: quinn::Endpoint, app: Router) -> Result<()> {
    while let Some(incoming) = endpoint.accept().await {
        let app = app.clone();
        tokio::spawn(async move {
            let result = async {
                let connection = incoming.await?;
                let mut connection = h3::server::Connection::<_, Bytes>::new(h3_quinn::Connection::new(connection)).await?;
                while let Some(request) = connection.accept().await? {
                    let app = app.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_request(request, app).await {
                            tracing::debug!(%error, "HTTP/3 request ended");
                        }
                    });
                }
                Ok::<_, anyhow::Error>(())
            }.await;
            if let Err(error) = result {
                tracing::debug!(%error, "HTTP/3 connection ended");
            }
        });
    }
    Ok(())
}

async fn handle_request(
    resolver: h3::server::RequestResolver<h3_quinn::Connection, Bytes>,
    app: Router,
) -> Result<()> {
    let (request, stream) = resolver.resolve_request().await?;
    let (mut send, recv) = stream.split();
    if request.method() == Method::CONNECT {
        send.send_response(Response::builder().version(Version::HTTP_3)
            .status(StatusCode::NOT_IMPLEMENTED).body(())?).await?;
        send.finish().await?;
        return Ok(());
    }
    let (mut parts, _) = request.into_parts();
    parts.version = Version::HTTP_3;
    let body = Body::from_stream(try_unfold(recv, |mut recv| async move {
        let chunk = recv.recv_data().await?.map(|mut data| data.copy_to_bytes(data.remaining()));
        Ok::<_, h3::error::StreamError>(chunk.map(|data| (data, recv)))
    }));
    let response = app.oneshot(axum::http::Request::from_parts(parts, body)).await?;
    let (mut parts, body) = response.into_parts();
    parts.version = Version::HTTP_3;
    send.send_response(Response::from_parts(parts, ())).await?;
    let mut data = body.into_data_stream();
    while let Some(chunk) = data.next().await {
        let chunk = chunk?;
        if !chunk.is_empty() { send.send_data(chunk).await?; }
    }
    send.finish().await?;
    Ok(())
}
