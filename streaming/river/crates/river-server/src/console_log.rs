use std::fmt;
use tracing::{Event, Subscriber, field::{Field, Visit}};
use tracing_subscriber::{fmt::{FmtContext, FormatEvent, FormatFields, format::Writer}, registry::LookupSpan};

pub const REQUEST_SPAN: &str = "river.http.request";
pub const SUMMARY_TARGET: &str = "river_server::http_summary";

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level { Off, Error, Warn, Info, Debug, Trace }

impl std::str::FromStr for Level {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> anyhow::Result<Self> {
        Ok(match value.to_ascii_lowercase().as_str() {
            "off" => Self::Off, "error" => Self::Error, "warn" => Self::Warn,
            "info" => Self::Info, "debug" => Self::Debug, "trace" => Self::Trace,
            _ => anyhow::bail!("Expected off, error, warn, info, debug or trace"),
        })
    }
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off", Self::Error => "error", Self::Warn => "warn",
            Self::Info => "info", Self::Debug => "debug", Self::Trace => "trace",
        }
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub minimum_level: Level,
    pub request_summary: bool,
    pub verbose: bool,
    pub pretty_print: bool,
    pub otlp_logs: bool,
    pub unprocessable_level: Level,
}

impl Default for Settings {
    fn default() -> Self {
        Self { minimum_level: Level::Info, request_summary: true, verbose: false, pretty_print: false, otlp_logs: false, unprocessable_level: Level::Warn }
    }
}

pub struct Console { pub verbose: bool, pub pretty: bool }

#[derive(Default)]
struct Fields(Vec<(&'static str, String)>);
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.push((field.name(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name(), value.to_owned()));
    }
}

impl<S, N> FormatEvent<S, N> for Console
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let metadata = event.metadata();
        let request_scoped = ctx.event_scope().is_some_and(|mut scope| scope.any(|span| span.name() == REQUEST_SPAN));
        if request_scoped && !self.verbose && metadata.target() != SUMMARY_TARGET {
            return Ok(());
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
        if self.pretty {
            let message = fields.0.iter().find(|(key, _)| *key == "message").map_or("River event", |(_, value)| value.as_str());
            writeln!(writer, "[{}] {}  ({} ms since epoch)", metadata.level(), message.escape_default(), timestamp)?;
            writeln!(writer, "  {:<24} {}", "target", metadata.target())?;
            for (name, value) in fields.0.iter().filter(|(name, _)| *name != "message") {
                writeln!(writer, "  {:<24} {:?}", name.replace('_', " "), value)?;
            }
            return Ok(());
        }
        write!(writer, "timestamp_ms={timestamp} level={} target={}", metadata.level(), metadata.target())?;
        fields.0.sort_by_key(|(name, _)| *name != "message");
        for (name, value) in fields.0 {
            // Quote and escape fields so one event always occupies one line.
            write!(writer, " {name}={value:?}")?;
        }
        writeln!(writer)
    }
}

static SETTINGS: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();

pub fn initialize(configured: Option<Settings>) -> anyhow::Result<Settings> {
    fn read<T: std::str::FromStr>(name: &str, fallback: T) -> anyhow::Result<T> {
        match std::env::var(name) {
            Ok(value) => value.parse().map_err(|_| anyhow::anyhow!("Invalid value for {name}")),
            Err(std::env::VarError::NotPresent) => Ok(fallback),
            Err(error) => Err(error.into()),
        }
    }
    let configured = configured.unwrap_or_default();
    let settings = Settings {
        minimum_level: read("RIVER_LOG_LEVEL", configured.minimum_level)?,
        request_summary: read("RIVER_LOG_REQUEST_SUMMARY", configured.request_summary)?,
        verbose: read("RIVER_LOG_VERBOSE", configured.verbose)?,
        pretty_print: read("RIVER_PRETTY_PRINT_LOGS", configured.pretty_print)?,
        otlp_logs: read("RIVER_OTEL_LOGS_ENABLED", configured.otlp_logs)?,
        unprocessable_level: read("RIVER_LOG_UNPROCESSABLE_LEVEL", configured.unprocessable_level)?,
    };
    SETTINGS.get_or_init(|| settings.clone());
    Ok(settings)
}

pub struct StructuredLogs {
    logger: opentelemetry_sdk::logs::SdkLogger,
    verbose: bool,
}

impl StructuredLogs {
    pub fn new(provider: &opentelemetry_sdk::logs::SdkLoggerProvider, verbose: bool) -> Self {
        use opentelemetry::logs::LoggerProvider;
        Self { logger: provider.logger("river-server"), verbose }
    }
}

impl<S> tracing_subscriber::Layer<S> for StructuredLogs
where S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        use opentelemetry::logs::{LogRecord, Logger, Severity};
        let metadata = event.metadata();
        if !metadata.target().starts_with("river_server") { return; }
        let request_scoped = ctx.event_scope(event).is_some_and(|mut scope| scope.any(|span| span.name() == REQUEST_SPAN));
        if request_scoped && !self.verbose && metadata.target() != SUMMARY_TARGET { return; }
        let mut record = self.logger.create_log_record();
        record.set_target(metadata.target());
        record.set_timestamp(std::time::SystemTime::now());
        let (severity, level) = match *metadata.level() {
            tracing::Level::ERROR => (Severity::Error, "ERROR"),
            tracing::Level::WARN => (Severity::Warn, "WARN"),
            tracing::Level::INFO => (Severity::Info, "INFO"),
            tracing::Level::DEBUG => (Severity::Debug, "DEBUG"),
            tracing::Level::TRACE => (Severity::Trace, "TRACE"),
        };
        record.set_severity_number(severity);
        record.set_severity_text(level);
        struct Visitor<'a, R>(&'a mut R);
        impl<R: LogRecord> Visit for Visitor<'_, R> {
            fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) { self.record_str(field, &format!("{value:?}")); }
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" { self.0.set_body(value.to_owned().into()); }
                else { self.0.add_attribute(field.name(), value.to_owned()); }
            }
            fn record_i64(&mut self, field: &Field, value: i64) { self.0.add_attribute(field.name(), value); }
            fn record_u64(&mut self, field: &Field, value: u64) {
                if let Ok(value) = i64::try_from(value) { self.record_i64(field, value); }
                else { self.record_str(field, &value.to_string()); }
            }
            fn record_f64(&mut self, field: &Field, value: f64) { self.0.add_attribute(field.name(), value); }
            fn record_bool(&mut self, field: &Field, value: bool) { self.0.add_attribute(field.name(), value); }
        }
        event.record(&mut Visitor(&mut record));
        self.logger.emit(record);
    }
}

pub fn summarize(response: &axum::response::Response, request_id: &str, route: &str, method: &str,
                 error_code: &str, started: std::time::Instant) {
    let settings = SETTINGS.get_or_init(Settings::default);
    if !settings.request_summary { return; }
    let status = response.status();
    let headers = response.headers();
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let timing = |name: &str| text(name).and_then(|value| value.parse::<f64>().ok());
    let count = |name: &str| text(name).and_then(|value| value.parse::<u64>().ok());
    let elapsed_ms = (started.elapsed().as_secs_f64() * 1_000_000.0).round() / 1_000.0;
    let level = if status.as_u16() == 422 {
        settings.unprocessable_level
    } else if status.is_server_error() {
        Level::Error
    } else if status.is_client_error() {
        Level::Warn
    } else {
        Level::Info
    };
    let message = if status.is_client_error() || status.is_server_error() {
        "HTTP request failed"
    } else {
        "HTTP request completed"
    };
    macro_rules! conclusion {
        ($level:expr) => {
            tracing::event!(target: SUMMARY_TARGET, $level, %request_id, %route, %method,
                status = status.as_u16(), %error_code, elapsed_ms,
                trace_id = text("x-river-trace-id"),
                selected_driver = text("x-river-selected-driver"),
                decoder_backend = text("x-river-decoder-backend"),
                decoder_route = text("x-river-decoder-route"),
                queue_ms = timing("x-river-queue-ms"),
                decode_ms = timing("x-river-decode-ms"),
                encode_ms = timing("x-river-encode-ms"),
                server_ms = timing("x-river-server-ms"),
                source_cache = text("x-river-source-cache"),
                source_cache_ms = timing("x-river-source-cache-ms"),
                source_read_bytes = count("x-river-source-read-bytes"),
                source_range_requests = count("x-river-source-range-requests"),
                source_cache_hit_bytes = count("x-river-source-cache-hit-bytes"),
                content_type = text("content-type"),
                content_length = count("content-length"),
                message = message)
        };
    }
    match level {
        Level::Off => {}
        Level::Error => conclusion!(tracing::Level::ERROR),
        Level::Warn => conclusion!(tracing::Level::WARN),
        Level::Info => conclusion!(tracing::Level::INFO),
        Level::Debug => conclusion!(tracing::Level::DEBUG),
        Level::Trace => conclusion!(tracing::Level::TRACE),
    }
}
