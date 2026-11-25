//! Share immutable, possibly overlapping reads without holding an image lock over I/O.
use super::*;
use std::collections::BTreeMap;
type Reply = Option<Result<bytes::Bytes, Arc<str>>>;
type Transfer = watch::Sender<Reply>;

#[derive(Default, Debug)]
pub(super) struct Flights(Mutex<BTreeMap<u64, Active>>);
#[derive(Debug)]
struct Active {
    end: u64,
    sender: Transfer,
    users: usize,
    finished: bool,
}
struct Claims {
    flights: Arc<Flights>,
    starts: Vec<(u64, Transfer)>,
}
impl Drop for Claims {
    fn drop(&mut self) {
        let mut active = self.flights.0.lock().unwrap_or_else(|e| e.into_inner());
        for (start, sender) in &self.starts {
            if let Some(entry) = active
                .get_mut(start)
                .filter(|entry| entry.sender.same_channel(sender))
            {
                entry.users -= 1;
                if entry.users == 0 && entry.finished {
                    active.remove(start);
                }
            }
        }
    }
}

pub(super) struct Fetched {
    _claims: Claims,
    pub data: bytes::Bytes,
    pub origin_bytes: u64,
    pub requests: u64,
}
impl std::ops::Deref for Fetched {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl Fetched {
    pub fn account(&self, stats: &mut ReadStats) {
        stats.requests += self.requests;
        stats.bytes += self.origin_bytes;
        stats.cache_bytes += self.len() as u64 - self.origin_bytes;
    }
}
impl RangeClient {
    pub(super) async fn shared_read(
        &self,
        range: Range<u64>,
        deadline: Instant,
        mut cancel: Option<watch::Receiver<bool>>,
    ) -> Result<Fetched> {
        let mut claims = Claims {
            flights: self.flights.clone(),
            starts: Vec::new(),
        };
        let mut pieces = Vec::new();
        let mut launch = Vec::new();
        {
            let mut active = self.flights.0.lock().unwrap_or_else(|e| e.into_inner());
            let mut pos = range.start;
            while pos < range.end {
                if let Some((&start, entry)) = active
                    .range_mut(..=pos)
                    .next_back()
                    .filter(|(_, entry)| entry.end > pos)
                {
                    let stop = entry.end.min(range.end);
                    entry.users += 1;
                    claims.starts.push((start, entry.sender.clone()));
                    pieces.push((pos - start..stop - start, entry.sender.subscribe()));
                    pos = stop;
                    continue;
                }
                let stop = active
                    .range(pos..range.end)
                    .next()
                    .map_or(range.end, |(&start, _)| start);
                let (tx, rx) = watch::channel(None);
                active.insert(
                    pos,
                    Active {
                        end: stop,
                        sender: tx.clone(),
                        users: 1,
                        finished: false,
                    },
                );
                claims.starts.push((pos, tx.clone()));
                pieces.push((0..stop - pos, rx));
                launch.push((pos..stop, tx));
                pos = stop;
            }
        }
        let requests = launch.len() as u64;
        let origin_bytes = launch.iter().map(|(r, _)| r.end - r.start).sum();
        for (span, tx) in launch {
            let client = self.clone();
            tokio::spawn(async move {
                // Each caller has its own deadline. A departing owner cannot cancel a joiner's transfer.
                let transfer = client.fetch_with_network_timeout(
                    span.clone(),
                    Instant::now() + Duration::from_secs(30),
                    None,
                    Duration::from_secs(10),
                );
                tokio::pin!(transfer);
                let result = loop {
                    tokio::select! {
                        result=&mut transfer=>break result,
                        _=tx.closed()=>{
                            let mut active=client.flights.0.lock().unwrap_or_else(|e|e.into_inner());
                            if tx.receiver_count()==0 {active.remove(&span.start);return;}
                        }
                    }
                };
                let mut active = client.flights.0.lock().unwrap_or_else(|e| e.into_inner());
                if result.is_err() || active.get(&span.start).is_none_or(|entry| entry.users == 0) {
                    active.remove(&span.start);
                } else if let Some(entry) = active.get_mut(&span.start) {
                    entry.finished = true;
                }
                tx.send_replace(Some(
                    result
                        .map(bytes::Bytes::from)
                        .map_err(|e| Arc::from(format!("{e:#}"))),
                ));
            });
        }
        let collect = async {
            let single = pieces.len() == 1;
            let mut bytes = Vec::new();
            for (part, mut rx) in pieces {
                let reply = rx
                    .wait_for(Option::is_some)
                    .await
                    .context("shared range worker stopped")?;
                let data = reply
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                if single {
                    return Ok::<_, anyhow::Error>(
                        data.slice(part.start as usize..part.end as usize),
                    );
                }
                bytes.extend_from_slice(&data[part.start as usize..part.end as usize]);
            }
            Ok(bytes.into())
        };
        let data = tokio::select! {
            result=tokio::time::timeout(deadline.saturating_duration_since(Instant::now()),collect)=>result.context("shared range deadline expired")?,
            _=async {if let Some(rx)=&mut cancel {if !*rx.borrow() {let _=rx.changed().await;}}else{std::future::pending::<()>().await;}}=>anyhow::bail!("S3 request superseded"),
        }?;
        Ok(Fetched {
            _claims: claims,
            data,
            requests,
            origin_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn overlapping_waiter_keeps_transfer_when_owner_leaves() {
        let store = Arc::new(object_store::memory::InMemory::new());
        let path = Path::from("overlap");
        let bytes: Vec<_> = (0..64).collect();
        store.put(&path, bytes.clone().into()).await.unwrap();
        let profile = Arc::new(crate::read_policy::Profile::default());
        let mut permits = Vec::new();
        for _ in 0..crate::read_policy::settings().effective_source_requests() {
            permits.push(profile.acquire().await);
        }
        let client = RangeClient {
            stage: "batchedRange",
            flights: Arc::default(),
            feedback: Arc::default(),
            budget: wire_budget::Budget::new(4),
            trace_id: 0,
            object_id: "overlap".into(),
            caller: std::panic::Location::caller(),
            block_size: 1,
            part: None,
            store,
            location: path,
            etag: None,
            version: None,
            network_min_us: Arc::new(AtomicU64::new(u64::MAX)),
            profile: profile.clone(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let (tx, rx) = watch::channel(false);
        let first = client.clone();
        let owner = tokio::spawn(async move { first.fetch(0..32, deadline, Some(rx)).await });
        while client.flights.0.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        let second = client.clone();
        let waiter = tokio::spawn(async move { second.fetch(16..48, deadline, None).await });
        while client.flights.0.lock().unwrap().len() != 2 {
            tokio::task::yield_now().await;
        }
        tx.send(true).unwrap();
        assert!(owner.await.unwrap().is_err());
        drop(permits);
        let result = waiter.await.unwrap().unwrap();
        assert_eq!(&*result, &bytes[16..48]);
        assert_eq!((result.requests, result.origin_bytes), (1, 16));
        assert_eq!(profile.snapshot().bytes, 48);
        drop(result);
        assert!(client.flights.0.lock().unwrap().is_empty());
    }
}
