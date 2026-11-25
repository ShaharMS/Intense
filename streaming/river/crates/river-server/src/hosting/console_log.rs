use std::fmt;
use tracing::{Event, Subscriber, field::{Field, Visit}};
use tracing_subscriber::registry::LookupSpan;
use std::io::Write;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

#[derive(Clone)]
pub struct FailureReason(pub String);

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

#[derive(Clone, Debug)]
pub struct TileAddress { pub asset:String, pub matrix:u8, pub col:u32, pub row:u32, pub layers:u32 }
#[derive(Clone, Debug)]
pub struct TileInfo { pub output_bytes:usize, pub width:u32, pub height:u32 }

pub struct Console {
    verbose: bool,
    sender: tokio::sync::mpsc::Sender<Record>,
    dropped: Arc<AtomicU64>,
}
struct Record {
    level: tracing::Level, target: &'static str, timestamp: u128, fields: Fields, page: bool,
}

#[derive(Default)]
struct Fields { values: Vec<(&'static str, String)>, detail: bool }
impl Visit for Fields {
    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "console_detail" { self.detail = value; }
        else { self.values.push((field.name(), value.to_string())); }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.values.push((field.name(), format!("{value:?}")));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.values.push((field.name(), value.to_owned()));
    }
}

impl Console {
    pub fn start(settings: &Settings) -> std::io::Result<Self> {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Record>(1024);
        let dropped = Arc::new(AtomicU64::new(0));
        let missed = dropped.clone();
        let pretty = settings.pretty_print;
        std::thread::Builder::new().name("river-console".into()).spawn(move || {
            let stdout = std::io::stdout();
            let mut page = Vec::with_capacity(4096);
            while let Some(record) = receiver.blocking_recv() {
                page.clear();
                let skipped = missed.swap(0, Ordering::Relaxed);
                if skipped > 0 { let _ = writeln!(page, "River console dropped {skipped} events while its queue was full"); }
                if record.write(&mut page, pretty).is_ok() { let _ = stdout.lock().write_all(&page); }
            }
        })?;
        Ok(Self { verbose: settings.verbose, sender, dropped })
    }
}

impl<S> tracing_subscriber::Layer<S> for Console
where S: Subscriber + for<'a> LookupSpan<'a> {
    fn on_event(&self, event: &Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let meta = event.metadata();
        if !self.verbose && meta.target() != SUMMARY_TARGET &&
            ctx.event_scope(event).is_some_and(|mut scope| scope.any(|s| s.name()==REQUEST_SPAN)) { return; }
        let Ok(slot) = self.sender.try_reserve() else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let page = self.verbose && meta.target()==SUMMARY_TARGET && fields.detail;
        slot.send(Record { level:*meta.level(), target:meta.target(), fields, page,
            timestamp:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() });
    }
}

impl Record {
    fn write(&self, out:&mut impl Write, pretty:bool) -> std::io::Result<()> {
        let fields=&self.fields.values;
        let get=|key:&str| fields.iter().find(|(name,_)| *name==key).map(|(_,v)|v.as_str());
        let message=get("message").unwrap_or("River event");
        if self.page {
            writeln!(out,"------------------------------------------------------------")?;
            writeln!(out,"{}  {}  {}",self.level,message.escape_default(),self.timestamp)?;
            for (key, value) in fields {
                if matches!(*key,"message"|"stages"|"source_stages") {continue;}
                writeln!(out,"  {:<24} {}",key.replace('_'," "),value.escape_default())?;
            }
            if let Some(stages)=get("stages") {
                let elapsed=get("elapsed_ms").and_then(|v|v.parse::<f64>().ok()).unwrap_or(0.0);
                writeln!(out,"  {:<24} {:>9} {:>12}","foreground stage","ms","% of HTTP")?;
                for metric in stages.split(", ") {
                    if let Some((key,ms))=metric.split_once(";dur=") {
                        if let Ok(value)=ms.parse::<f64>() {
                            let percent=if elapsed>0.0 {100.0*value/elapsed} else {0.0};
                            writeln!(out,"  {:<24} {:>9.3} {:>11.1}%",key,value,percent)?;
                        }
                    }
                }
            }
            if let Some(sources)=get("source_stages") {
                writeln!(out,"  {:<36} {:>7} {:>14} {:>14}","source stage","GETs","origin bytes","cache bytes")?;
                for entry in sources.split(", ") {
                    let mut fields=entry.split(';');
                    let stage=fields.next().unwrap_or("");
                    let mut gets=""; let mut bytes=""; let mut cache="";
                    let mut extra=String::new();
                    for field in fields {
                        match field.split_once('=') {
                            Some(("gets",v))=>gets=v,
                            Some(("bytes",v))=>bytes=v,
                            Some(("cache_bytes",v))=>cache=v,
                            Some(("curl_requests","0"))=>{},
                            _=>{extra.push(' ');extra.push_str(field);}
                        }
                    }
                    writeln!(out,"  {stage:<36} {gets:>7} {bytes:>14} {cache:>14}{extra}")?;
                }
            }
            writeln!(out,"------------------------------------------------------------")
        } else if pretty {
            writeln!(out,"[{}] {}  ({} ms since epoch)",self.level,message.escape_default(),self.timestamp)?;
            writeln!(out,"  {:<24} {}","target",self.target)?;
            for (key,value) in fields.iter().filter(|(k,_)| *k!="message") {
                writeln!(out,"  {:<24} {:?}",key.replace('_'," "),value)?;
            }
            Ok(())
        } else {
            write!(out,"timestamp_ms={} level={} target={} message={message:?}",self.timestamp,self.level,self.target)?;
            for (key,value) in fields.iter().filter(|(k,_)| *k!="message") {write!(out," {key}={value:?}")?;}
            writeln!(out)
        }
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
    let failure=response.extensions().get::<crate::errors::TileFailure>();
    let diagnostic_only = !settings.request_summary && failure.is_none();
    if diagnostic_only && !(tracing::enabled!(target: SUMMARY_TARGET, tracing::Level::DEBUG)
        && response.headers().contains_key("server-timing")) { return; }
    let status = response.status();
    let headers = response.headers();
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let timing = |name: &str| text(name).and_then(|value| value.parse::<f64>().ok());
    let count = |name: &str| text(name).and_then(|value| value.parse::<u64>().ok());
    let elapsed_ms = (started.elapsed().as_secs_f64() * 1_000_000.0).round() / 1_000.0;
    let level = if diagnostic_only { Level::Debug } else if status.as_u16() == 422 {
        settings.unprocessable_level
    } else if status.is_server_error() {
        Level::Error
    } else if status.is_client_error() {
        Level::Warn
    } else {
        Level::Info
    };
    let message = if diagnostic_only { "WMTS tile diagnostic" } else if status.is_client_error() || status.is_server_error() {
        "HTTP request failed"
    } else {
        "HTTP request completed"
    };
    let console_detail = settings.verbose && tracing::enabled!(target: SUMMARY_TARGET, tracing::Level::DEBUG);
    let address = response.extensions().get::<TileAddress>();
    let tile = response.extensions().get::<TileInfo>();
    macro_rules! conclusion {
        ($level:expr) => {
            tracing::event!(target: SUMMARY_TARGET, $level, console_detail, %request_id, %route, %method,
                status = status.as_u16(), %error_code, elapsed_ms,
                failure_reason = response.extensions().get::<FailureReason>().map(|e|e.0.as_str()),
                failure_details = failure.map(|e|tracing::field::display(&e.0)),
                trace_id = text("x-river-trace-id"),
                selected_driver = text("x-river-selected-driver"),
                decoder_backend = text("x-river-decoder-backend"),
                decoder_route = text("x-river-decoder-route"),
                queue_ms = timing("x-river-queue-ms"),
                decode_ms = timing("x-river-decode-ms"),
                grok_native_ms = timing("x-river-grok-native-ms"),
                grok_decompress_ms = timing("x-river-grok-decompress-ms"),
                grok_conversion_ms = timing("x-river-grok-conversion-ms"),
                block_hits = count("x-river-block-hits"), block_misses = count("x-river-block-misses"),
                encode_ms = timing("x-river-encode-ms"),
                stages = text("server-timing"),
                source_stages = text("x-river-source-stages"),
                prefix_plan = text("x-river-prefix-plan"),
                index_source = text("x-river-index-source"),
                asset = address.map(|t|t.asset.as_str()),
                matrix = address.map(|t|t.matrix as u64), column = address.map(|t|t.col), row = address.map(|t|t.row),
                layers = address.map(|t|t.layers), output_width = tile.map(|t|t.width), output_height = tile.map(|t|t.height),
                source_cache = text("x-river-source-cache"),
                source_cache_ms = timing("x-river-source-cache-ms"),
                source_read_bytes = count("x-river-source-read-bytes"),
                source_range_requests = count("x-river-source-range-requests"),
                source_cache_hit_bytes = count("x-river-source-cache-hit-bytes"),
                content_type = text("content-type"),
                content_length = tile.map(|t|t.output_bytes as u64).or_else(||count("content-length")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn stalled_console_drops_events_and_resumes_whole_summaries() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let dropped = Arc::new(AtomicU64::new(0));
        let console = Console { verbose: true, sender, dropped: dropped.clone() };
        let subscriber = tracing_subscriber::registry().with(console);
        tracing::subscriber::with_default(subscriber, || {
            let detail = tracing::enabled!(target: SUMMARY_TARGET, tracing::Level::DEBUG);
            tracing::info!(target: SUMMARY_TARGET, console_detail = detail, elapsed_ms = 8.0, stages = "prepare;dur=2.000", source_stages = "source_prefix_probe_read;gets=3;bytes=3072;cache_bytes=1024;curl_requests=0", "request");
            tracing::debug!("event during backpressure");
            assert_eq!(dropped.load(Ordering::Relaxed), 1);
            let mut output = Vec::new();
            receiver.try_recv().unwrap().write(&mut output, false).unwrap();
            let page = String::from_utf8(output).unwrap();
            assert_eq!(page.lines().filter(|line| line.starts_with("-----")).count(), 2);
            assert!(page.contains("25.0%"));
            assert!(page.lines().any(|line| line.split_whitespace().eq(
                ["source_prefix_probe_read", "3", "3072", "1024"])));

            tracing::debug!("event after backpressure");
            assert!(receiver.try_recv().is_ok());
        });
    }
}
