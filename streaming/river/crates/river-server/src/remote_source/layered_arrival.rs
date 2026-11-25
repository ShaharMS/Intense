//! Observe the normal PLT fetch. No extra ranges or waiting for missing bytes.
use super::*;
use std::sync::{atomic::AtomicUsize, mpsc};

// The tile part is a contiguous, bounded extent. Index arrivals by physical
// block number instead of hashing each codeword fragment at every completion.
struct Blocks {
    first: u64,
    slots: Vec<Option<Arc<[u8]>>>,
}
impl Blocks {
    fn get(&self, index: u64) -> Option<&[u8]> {
        self.slots
            .get(usize::try_from(index.checked_sub(self.first)?).ok()?)?
            .as_deref()
    }
    fn copy(&self, size: u64, range: Range<u64>, out: &mut Vec<u8>) -> Result<()> {
        let mut pos = range.start;
        while pos < range.end {
            let data = self.get(pos / size).context("missing arrival block")?;
            let within = (pos % size) as usize;
            let n = (range.end - pos).min(data.len().saturating_sub(within) as u64) as usize;
            ensure!(n > 0, "short arrival block");
            out.extend_from_slice(&data[within..within + n]);
            pos += n as u64;
        }
        Ok(())
    }
}
struct Word {
    key: [u32; 8],
    passes: u32,
    pieces: Vec<Range<u64>>,
}
struct Group {
    grids: Vec<(usize, usize)>,
    packets: Vec<Range<u64>>,
    geometry: Vec<Vec<Option<[u32; 8]>>>,
    planes: Vec<u32>,
}

fn available(blocks: &Blocks, size: u64, r: &Range<u64>) -> bool {
    (r.start / size..r.end.div_ceil(size)).all(|i| blocks.get(i).is_some())
}

impl Group {
    // Every layer header is required to prove the last contribution. A missing
    // header retries on the next completion; unsupported syntax uses normal decode.
    fn words(&self, blocks: &Blocks, size: u64) -> Result<Option<Vec<Word>>> {
        let count: usize = self.grids.iter().map(|(w, h)| w * h).sum();
        let probes: Vec<_> = self
            .packets
            .iter()
            .map(|r| r.start..r.end.min(r.start + 64 + 8 * count as u64))
            .collect();
        if !probes.iter().all(|r| available(blocks, size, r)) {
            return Ok(None);
        }
        let mut state = layered_packets::State::new_lazy(&self.grids, 0);
        let mut bands: Vec<Vec<_>> = self
            .geometry
            .iter()
            .map(|g| {
                g.iter()
                    .map(|k| {
                        k.map(|key| Word {
                            key,
                            passes: 0,
                            pieces: vec![],
                        })
                    })
                    .collect()
            })
            .collect();
        for (layer, (range, probe)) in self.packets.iter().zip(probes).enumerate() {
            let mut header = Vec::new();
            blocks.copy(size, probe, &mut header)?;
            let mut contributions = vec![];
            let (n, body) = state.contributions(&header, layer as u16, |b, i, z, p, l, h| {
                contributions.push((b, i, z, p, l, h))
            })?;
            ensure!(
                n as u64 + body == range.end - range.start,
                "layer packet length mismatch"
            );
            let start = range.start + n as u64;
            for (b, i, z, p, l, h) in contributions {
                if let Some(w) = &mut bands[b][i] {
                    w.key[7] = self.planes[b]
                        .checked_sub(z)
                        .context("invalid bit planes")?;
                    w.passes = p as u32;
                    if h > l {
                        w.pieces.push(start + l..start + h)
                    }
                }
            }
        }
        Ok(Some(
            bands
                .into_iter()
                .flatten()
                .flatten()
                .filter(|w| !w.pieces.is_empty())
                .collect(),
        ))
    }
}

impl RemoteSource {
    fn arrival_groups(
        &self,
        tile: u16,
        spans: &[(bool, Range<u64>)],
        planes: &[u32],
        pos: (u32, u32),
        edge: u32,
        ctx: &mut ReadContext,
    ) -> Option<Vec<Group>> {
        let h = &self.header;
        let (order, _) = self.ordered_packets(tile, None, ctx).ok()?;
        if order.len() != spans.len() {
            return None;
        }
        let (tx, ty, ex, ey) = h.tile_bounds(tile);
        let (x, y) = (pos.0.checked_mul(edge)?, pos.1.checked_mul(edge)?);
        let mut groups = BTreeMap::new();
        let mut num_blocks = 0;
        for (packet, (keep, span)) in order.iter().zip(spans) {
            if !keep {
                return None;
            }
            let r = packet.resolution;
            let k = (r, packet.component, packet.x, packet.y);
            if packet.layer == 0 {
                let grids = prefix_codeblocks(h, tile, packet).ok()??;
                if grids.len() != if r == 0 { 1 } else { 3 } {
                    return None;
                }
                let mut geometry = vec![];
                let mut band_planes = vec![];
                for (b, &(w, ht)) in grids.iter().enumerate() {
                    num_blocks += w * ht;
                    if num_blocks > 4096 {
                        return None;
                    }
                    let band = if r == 0 { 0 } else { b + 1 };
                    let scale = 1u32 << (h.levels - 1 - r + u8::from(r > 0));
                    let project = |v: u32, high: usize| {
                        v.saturating_sub((high as u32) * (scale / 2))
                            .div_ceil(scale)
                    };
                    let (bx, by) = (project(tx, band & 1), project(ty, band >> 1));
                    let (endx, endy) = (project(ex, band & 1), project(ey, band >> 1));
                    let (cw, ch) = (1u32 << (h.cod[6] + 2), 1u32 << (h.cod[7] + 2));
                    if (endx.div_ceil(cw) - bx / cw) as usize != w
                        || (endy.div_ceil(ch) - by / ch) as usize != ht
                    {
                        return None;
                    }
                    let (rx, ry) = (
                        project(x, band & 1).saturating_sub(2),
                        project(y, band >> 1).saturating_sub(2),
                    );
                    let (sx, sy) = (
                        project(x.saturating_add(edge), band & 1).saturating_add(2),
                        project(y.saturating_add(edge), band >> 1).saturating_add(2),
                    );
                    geometry.push(
                        (0..w * ht)
                            .map(|i| {
                                let x0 = (bx / cw + (i % w) as u32) * cw;
                                let y0 = (by / ch + (i / w) as u32) * ch;
                                let (x1, y1) = (
                                    x0.saturating_add(cw).min(endx),
                                    y0.saturating_add(ch).min(endy),
                                );
                                let (x0, y0) = (x0.max(bx), y0.max(by));
                                (x0 < sx && y0 < sy && x1 > rx && y1 > ry).then_some([
                                    packet.component as u32,
                                    r as u32,
                                    band as u32,
                                    x0,
                                    y0,
                                    x1 - x0,
                                    y1 - y0,
                                    0,
                                ])
                            })
                            .collect(),
                    );
                    band_planes.push(*planes.get(if r == 0 {
                        0
                    } else {
                        3 * (r as usize - 1) + band
                    })?);
                }
                groups.insert(
                    k,
                    Group {
                        grids,
                        packets: vec![],
                        geometry,
                        planes: band_planes,
                    },
                );
            }
            let group = groups.get_mut(&k)?;
            if group.packets.len() != packet.layer as usize {
                return None;
            }
            group.packets.push(span.clone());
        }
        groups
            .values()
            .all(|g| g.packets.len() == h.layers as usize)
            .then(|| groups.into_values().collect())
    }

    pub(super) fn layered_payload(
        &self,
        main: &[u8],
        markers: &[u8],
        part: &TilePart,
        spans: &[(bool, Range<u64>)],
        pos: (u32, u32),
        edge: u32,
        ctx: &mut ReadContext,
    ) -> Result<Vec<u8>> {
        let h = &self.header;
        if ctx.planning_driver_grok != Some(false)
            || ctx.tier1_threads < 2
            || ctx.ready_blocks.is_some()
            || h.is_ht
            || !(2..=16).contains(&h.layers)
            || h.components > 4
            || h.levels > 16
            || h.image_origin != (0, 0)
            || h.cod[0] != 0
            || h.cod[8] != 0
            || h.cod[9] != 1
            || part.length > 8 << 20
            || !self.has_tier1_preview.load(Ordering::Relaxed)
            || [1, 2, 3, 4].into_iter().all(|quarter| {
                // If the part looks warm, ordinary preparation already checks
                // exact coverage. Sampling affects speculative CPU work only.
                let pos = part.offset + (part.length - 1) * quarter / 4;
                self.cache.contains(
                    &self.cache_key,
                    self.block_size,
                    pos / self.block_size * self.block_size,
                )
            })
        {
            return self.selected_payload(spans, ctx);
        }
        let Some(planes) = codeblock_roi::arrival::planes(main, markers, h.levels) else {
            return self.selected_payload(spans, ctx);
        };
        let Some(mut groups) = self.arrival_groups(part.tile, spans, &planes, pos, edge, ctx)
        else {
            return self.selected_payload(spans, ctx);
        };
        let lanes = crate::decoder::thread_budget::arrival_limit(ctx.tier1_threads);
        let leases: Vec<_> = (0..lanes)
            .map_while(|_| crate::decoder::thread_budget::try_arrival_worker())
            .collect();
        if leases.is_empty() {
            return self.selected_payload(spans, ctx);
        }
        let size = self.block_size;
        let first = part.offset / size;
        let n = ((part.offset + part.length).div_ceil(size) - first) as usize;
        let count = AtomicUsize::new(0);
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            let (tasks, rx) = mpsc::sync_channel::<([u32; 8], u32, Vec<u8>)>(leases.len() * 8);
            let rx = Arc::new(Mutex::new(rx));
            for lease in leases {
                let rx = rx.clone();
                let count = &count;
                let stop = &stop;
                let cancel = ctx.cancel.clone();
                let deadline = ctx.deadline;
                let revision = self.block_namespace;
                let _ = std::thread::Builder::new().spawn_scoped(scope, move || {
                    let _lease = lease;
                    let Some(mut decoder) = crate::decoder::block_reuse::WordDecoder::new() else {
                        return;
                    };
                    loop {
                        let job = rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                        let Ok((key, passes, bytes)) = job else { break };
                        if stop.load(Ordering::Acquire)
                            || Instant::now() >= deadline
                            || cancel.as_ref().is_some_and(|c| *c.borrow())
                        {
                            break;
                        }
                        if let Some(samples) = decoder.coefficients(&key, passes, &bytes) {
                            if crate::decoder::tier1_resume::publish(
                                revision, key, passes, bytes, samples,
                            ) {
                                count.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
            drop(rx);
            let (events, arrivals) = mpsc::channel::<Vec<(u64, Arc<[u8]>)>>();
            let done = &stop;
            let dispatcher = std::thread::Builder::new().spawn_scoped(scope, move || {
                let mut blocks = Blocks {
                    first,
                    slots: vec![None; n],
                };
                let mut waiting = vec![];
                for batch in arrivals {
                    if done.load(Ordering::Acquire) {
                        break;
                    }
                    for (offset, data) in batch {
                        if let Some(slot) = (offset / size)
                            .checked_sub(first)
                            .and_then(|i| blocks.slots.get_mut(i as usize))
                        {
                            *slot = Some(data);
                        }
                    }
                    groups.retain(|g| match g.words(&blocks, size) {
                        Ok(Some(words)) => {
                            waiting.extend(words);
                            false
                        }
                        Ok(None) => true,
                        Err(_) => false,
                    });
                    waiting.retain(|w| {
                        if done.load(Ordering::Acquire) {
                            return false;
                        }
                        if !w.pieces.iter().all(|r| available(&blocks, size, r)) {
                            return true;
                        }
                        let len = w.pieces.iter().map(|r| (r.end - r.start) as usize).sum();
                        let mut data = Vec::with_capacity(len);
                        if w.pieces
                            .iter()
                            .all(|r| blocks.copy(size, r.clone(), &mut data).is_ok())
                        {
                            let _ = tasks.send((w.key, w.passes, data));
                        }
                        false
                    });
                }
            });
            if dispatcher.is_err() {
                return self.selected_payload(spans, ctx);
            }
            // Close the event stream even if fetching fails. Workers give back
            // their CPU permits before the native decoder sizes its own pool.
            struct Guard<'a>(&'a mut ReadContext, &'a std::sync::atomic::AtomicBool);
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.1.store(true, Ordering::Release);
                    self.0.ready_blocks.take();
                }
            }
            ctx.ready_blocks = Some(events);
            let guard = Guard(ctx, &stop);
            let leases = self.prefetch_packets(spans, guard.0)?;
            let before_fetch_end = count.load(Ordering::Relaxed);
            let result = self.assemble_payload(spans, &leases, guard.0);
            tracing::debug!(
                before_fetch_end,
                completed_before_prepare = count.load(Ordering::Relaxed),
                "Multilayer arrival decode"
            );
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_layers_assemble_one_exact_segment() {
        let g = Group {
            grids: vec![(1, 1)],
            packets: vec![0..4, 8..19],
            geometry: vec![vec![Some([2, 3, 1, 64, 0, 64, 64, 0])]],
            planes: vec![9],
        };
        let mut blocks = Blocks {
            first: 0,
            slots: vec![None; 3],
        };
        blocks.slots[1] = Some(Arc::from([0xd4, 0x80, 4, 5, 6, 7, 8, 9]));
        blocks.slots[2] = Some(Arc::from([10, 11, 12]));
        assert!(g.words(&blocks, 8).unwrap().is_none());
        blocks.slots[0] = Some(Arc::from([0xe3, 1, 2, 3, 0, 0, 0, 0]));
        let words = g.words(&blocks, 8).unwrap().unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].passes, 2);
        assert_eq!(words[0].key, [2, 3, 1, 64, 0, 64, 64, 9]);
        let mut body = vec![];
        for r in &words[0].pieces {
            blocks.copy(8, r.clone(), &mut body).unwrap();
        }
        assert_eq!(body, (1..=12).collect::<Vec<_>>());
        blocks.slots[1] = Some(Arc::from([0, 0, 4, 5, 6, 7, 8, 9]));
        assert!(g.words(&blocks, 8).is_err());
    }
}
