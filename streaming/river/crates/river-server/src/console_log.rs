//! Bounded console lines. Event IDs keep split fields correlated under concurrency.
use std::{fmt, sync::atomic::{AtomicU64,Ordering}};
use tracing::{Event,Subscriber,field::{Field,Visit}};
use tracing_subscriber::{fmt::{FmtContext,FormatEvent,FormatFields,format::Writer},registry::LookupSpan};
static EVENT:AtomicU64=AtomicU64::new(1);
pub struct Console;
#[derive(Default)]struct Fields(Vec<(String,String)>);
impl Visit for Fields {
 fn record_debug(&mut self,f:&Field,v:&dyn fmt::Debug){self.0.push((f.name().into(),format!("{v:?}")));}
 fn record_str(&mut self,f:&Field,v:&str){self.0.push((f.name().into(),v.into()));}
}
fn lines(prefix:&str,key:&str,value:&str)->Vec<String> {
 // Escape embedded control characters so an error cannot inject unlabelled lines.
 let value=value.chars().flat_map(|c|if c.is_control(){c.escape_default().collect::<Vec<_>>()}else{vec![c]}).collect::<Vec<_>>();
 let label=format!("{prefix} {key}=");let width=160usize.saturating_sub(label.len()).max(4);
 if value.is_empty(){return vec![label];}
 let mut output=Vec::new();let mut part=String::new();
 for c in value {if part.len()+c.len_utf8()>width {output.push(format!("{label}{part}"));part.clear();}part.push(c);}
 if !part.is_empty(){output.push(format!("{label}{part}"));}output
}
impl<S,N> FormatEvent<S,N> for Console where S:Subscriber+for<'a>LookupSpan<'a>,N:for<'a>FormatFields<'a>+'static {
 fn format_event(&self,ctx:&FmtContext<'_,S,N>,mut writer:Writer<'_>,event:&Event<'_>)->fmt::Result {
  let id=EVENT.fetch_add(1,Ordering::Relaxed);
  let span=ctx.event_scope().and_then(|scope|scope.last()).map(|s|s.id().into_u64()).unwrap_or(0);
  let root=ctx.event_scope().and_then(|scope|scope.from_root().next()).map(|s|s.id().into_u64()).unwrap_or(0);
  let time=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis();
  let prefix=format!("t={time} {} e={id} r={root} s={span}",event.metadata().level());
  let mut fields=Fields::default();event.record(&mut fields);
  fields.0.sort_by_key(|(key,_)|key!="message");
  for (key,value) in fields.0 {for line in lines(&prefix,&key,&value){writeln!(writer,"{line}")?;}}
  Ok(())
 }
}
#[cfg(test)]mod tests {
 use super::*;
 #[test]fn long_unicode_fields_have_bounded_correlated_lines(){
  let text="שלום".repeat(300);let output=lines("WARN e=4 s=7","reason",&text);
  assert!(output.len()>1);assert!(output.iter().all(|l|l.len()<=160 && l.starts_with("WARN e=4 s=7 reason=")));
  assert_eq!(output.iter().map(|l|l.strip_prefix("WARN e=4 s=7 reason=").unwrap()).collect::<String>(),text);
 }
 #[test]fn control_characters_cannot_create_unlabelled_lines(){
  let output=lines("INFO e=1 s=0","message","a\nb\r\u{1b}[31m");
  assert_eq!(output.len(),1);assert!(!output[0].contains('\n'));assert!(!output[0].contains('\u{1b}'));
 }
 #[test]fn formatter_omits_repeated_span_payload_and_bounds_real_events(){
  #[derive(Clone)]struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
  impl std::io::Write for Sink {
   fn write(&mut self,b:&[u8])->std::io::Result<usize>{self.0.lock().unwrap().extend_from_slice(b);Ok(b.len())}
   fn flush(&mut self)->std::io::Result<()>{Ok(())}
  }
  let bytes=std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));let output=bytes.clone();
  let subscriber=tracing_subscriber::fmt().with_ansi(false).event_format(Console).with_writer(move||Sink(output.clone())).finish();
  tracing::subscriber::with_default(subscriber,||{
   let span=tracing::info_span!("request",large_payload="SPAN_PAYLOAD_MUST_NOT_REPEAT");let _guard=span.enter();
   tracing::warn!(reason=%"x".repeat(900),"fallback");
  });
  let text=String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
  assert!(text.contains("message=fallback"));assert!(!text.contains("SPAN_PAYLOAD_MUST_NOT_REPEAT"));
  assert!(text.lines().all(|line|line.len()<=160 && line.contains(" e=") && line.contains(" r=")));
 }
}
