//! Native HTTPS listener. TLS paths and listener addresses are startup settings.
use std::{net::SocketAddr, path::PathBuf};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub cert_file: Option<PathBuf>,
    pub key_file: Option<PathBuf>,
    pub pfx_file: Option<PathBuf>,
    pub pfx_password_file: Option<PathBuf>,
    #[serde(default = "default_protocols")]
    pub protocols: Vec<Protocol>,
    pub http3_public_port: Option<u16>,
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
}

#[derive(Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Protocol {
    #[serde(rename = "http/1.1")]
    Http1,
    #[serde(rename = "h2")]
    H2,
    #[serde(rename = "h3")]
    H3,
}

fn default_protocols() -> Vec<Protocol> { vec![Protocol::Http1, Protocol::H2] }

fn default_bind() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8443))
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        let configured = |path: &Option<PathBuf>| path.as_ref().is_some_and(|p| !p.as_os_str().is_empty());
        if self.pfx_file.is_some() {
            ensure!(configured(&self.pfx_file) && self.cert_file.is_none() && self.key_file.is_none(),
                "TLS requires either a PFX file or a PEM certificate and key");
        } else {
            ensure!(configured(&self.cert_file) && configured(&self.key_file) && self.pfx_password_file.is_none(),
                "TLS requires a PEM certificate and key or a PFX file");
        }
        ensure!(self.bind.port() != 0 && self.http3_public_port != Some(0), "TLS ports must be nonzero");
        ensure!(!self.protocols.is_empty(), "Select at least one TLS protocol");
        for (index, protocol) in self.protocols.iter().enumerate() {
            ensure!(!self.protocols[..index].contains(protocol), "Duplicate TLS protocol");
        }
        Ok(())
    }
}

pub fn settings(configured: Option<Settings>) -> Result<Option<Settings>> {
    let enabled = match std::env::var("RIVER_TLS_ENABLED") {
        Ok(value) => Some(value.parse::<bool>().context("RIVER_TLS_ENABLED must be true or false")?),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error).context("Invalid RIVER_TLS_ENABLED"),
    };
    if enabled == Some(false) {
        return Ok(None);
    }
    let cert = std::env::var_os("RIVER_TLS_CERT_FILE");
    let key = std::env::var_os("RIVER_TLS_KEY_FILE");
    let pfx = std::env::var_os("RIVER_TLS_PFX_FILE");
    let bind = match std::env::var("RIVER_TLS_BIND") {
        Ok(value) => Some(value.parse::<SocketAddr>().context("RIVER_TLS_BIND must be a socket address")?),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error).context("Invalid RIVER_TLS_BIND"),
    };
    if enabled.is_none() && configured.is_none() && cert.is_none() && key.is_none() && pfx.is_none() && bind.is_none() {
        return Ok(None);
    }
    let mut effective = configured.unwrap_or_else(|| Settings {
        cert_file: None, key_file: None, pfx_file: None, pfx_password_file: None,
        bind: default_bind(), protocols: default_protocols(), http3_public_port: None,
    });
    ensure!(pfx.is_none() || (cert.is_none() && key.is_none()), "ENV cannot specify both PEM and PFX");
    if let Some(pfx) = pfx {
        effective.pfx_file = Some(pfx.into());
        effective.cert_file = None;
        effective.key_file = None;
    }
    if cert.is_some() || key.is_some() {
        effective.pfx_file = None;
        effective.pfx_password_file = None;
    }
    if let Some(cert) = cert { effective.cert_file = Some(cert.into()); }
    if let Some(key) = key { effective.key_file = Some(key.into()); }
    if let Some(password) = std::env::var_os("RIVER_TLS_PFX_PASSWORD_FILE") {
        effective.pfx_password_file = Some(password.into());
    }
    if let Some(bind) = bind { effective.bind = bind; }
    if let Ok(protocols) = std::env::var("RIVER_TLS_PROTOCOLS") {
        effective.protocols = protocols.split(',').map(|name| match name.trim() {
            "http/1.1" => Ok(Protocol::Http1),
            "h2" => Ok(Protocol::H2),
            "h3" => Ok(Protocol::H3),
            _ => anyhow::bail!("RIVER_TLS_PROTOCOLS accepts http/1.1, h2 and h3"),
        }).collect::<Result<_>>()?;
    }
    if let Ok(port) = std::env::var("RIVER_HTTP3_PUBLIC_PORT") {
        effective.http3_public_port = Some(port.parse().context("Invalid RIVER_HTTP3_PUBLIC_PORT")?);
    }
    effective.validate()?;
    Ok(Some(effective))
}

async fn load_identity(settings: &Settings) -> Result<RustlsConfig> {
    if let Some(pfx) = &settings.pfx_file {
        let mut password = std::ffi::OsString::from("pass:");
        if let Some(path) = &settings.pfx_password_file {
            password = "file:".into();
            password.push(path);
        }
        let extract = |flags: &'static [&'static str]| {
            let mut command = tokio::process::Command::new("openssl");
            command.args(["pkcs12", "-in"]).arg(pfx)
                .arg("-passin").arg(&password).args(flags);
            async move {
                let output = command.output().await.context("Could not run OpenSSL for PFX import")?;
                ensure!(output.status.success(), "Could not open PFX with the supplied password or extract its identity");
                Ok::<_, anyhow::Error>(output.stdout)
            }
        };
        let mut cert = extract(&["-clcerts", "-nokeys"]).await?;
        cert.extend(extract(&["-cacerts", "-nokeys"]).await?);
        let key = extract(&["-nocerts", "-noenc"]).await?;
        Ok(RustlsConfig::from_pem(cert, key).await?)
    } else {
        Ok(RustlsConfig::from_pem_file(settings.cert_file.as_ref().context("Missing PEM certificate")?,
                                      settings.key_file.as_ref().context("Missing PEM private key")?).await?)
    }
}

pub async fn serve<L>(http: L, app: Router, tls: Option<Settings>) -> Result<()>
where
    L: axum::serve::Listener,
    L::Addr: std::fmt::Debug,
{
    let Some(tls) = tls else {
        axum::serve(http, app).await?;
        return Ok(());
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let identity = load_identity(&tls).await.context("Could not load River TLS identity")?;
    let http_app = app.clone();
    let http1 = tls.protocols.contains(&Protocol::Http1);
    let h2 = tls.protocols.contains(&Protocol::H2);
    let h3 = tls.protocols.contains(&Protocol::H3);
    let quic = h3.then(|| super::http3::bind(tls.bind, &identity)).transpose()?;
    let tcp = if http1 || h2 {
        let mut config = (*identity.get_inner()).clone();
        config.alpn_protocols.clear();
        if h2 { config.alpn_protocols.push(b"h2".to_vec()); }
        if http1 { config.alpn_protocols.push(b"http/1.1".to_vec()); }
        let listener = std::net::TcpListener::bind(tls.bind)
            .with_context(|| format!("Could not bind River HTTPS on {}", tls.bind))?;
        listener.set_nonblocking(true)?;
        let mut server = axum_server::from_tcp_rustls(listener, RustlsConfig::from_config(config.into()))?;
        if h2 { server.http_builder().http2().enable_connect_protocol(); }
        Some(server)
    } else { None };
    let alt_svc = h3.then(|| axum::http::HeaderValue::from_str(&format!(
        "h3=\":{}\"; ma=86400", tls.http3_public_port.unwrap_or(tls.bind.port())))).transpose()?;
    let tcp_app = app.clone().layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
        let alt_svc = alt_svc.clone();
        async move {
            let supported = match request.version() {
                axum::http::Version::HTTP_2 => h2,
                axum::http::Version::HTTP_11 => http1,
                _ => false,
            };
            if !supported {
                return axum::response::IntoResponse::into_response(axum::http::StatusCode::HTTP_VERSION_NOT_SUPPORTED);
            }
            let mut response = next.run(request).await;
            if let Some(value) = alt_svc { response.headers_mut().insert("alt-svc", value); }
            response
        }
    }));
    tracing::info!(address=%tls.bind, http1, h2, h3, "River HTTPS listening");
    tokio::try_join!(
        async { axum::serve(http, http_app).await.context("River HTTP listener failed") },
        async {
            if let Some(server) = tcp { server.serve(tcp_app.into_make_service()).await.context("River HTTPS listener failed") }
            else { Ok(()) }
        },
        async {
            if let Some(endpoint) = quic { super::http3::serve(endpoint, app).await }
            else { Ok(()) }
        },
    )?;
    Ok(())
}
