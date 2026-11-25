//! Spatial extraction inside a large precinct. Rebuild Tier-2 headers only;
//! every retained Tier-1 codeword is copied verbatim from the source object.
use super::*;
pub(super) mod arrival;

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    remaining: u8,
}
impl Writer {
    fn bit(&mut self, value: u8) {
        if self.remaining == 0 {
            self.remaining = if self.bytes.last() == Some(&255) {
                7
            } else {
                8
            };
            self.bytes.push(0);
        }
        self.remaining -= 1;
        *self.bytes.last_mut().unwrap() |= value << self.remaining;
    }
    fn finish(mut self) -> Vec<u8> {
        if self.bytes.last() == Some(&255) {
            self.bytes.push(0);
        }
        self.bytes
    }
}

// A tag tree encodes minima from its root down. After a leaf is omitted its
// ancestors' minima may change, so copying the original tree bits is unsafe.
struct EncoderTree {
    levels: Vec<(usize, Vec<u32>, Vec<u32>)>,
}
impl EncoderTree {
    fn new(mut width: usize, mut height: usize, mut values: Vec<u32>) -> Self {
        let mut levels = Vec::new();
        loop {
            let mut parent = vec![u32::MAX; width.div_ceil(2) * height.div_ceil(2)];
            for (i, &v) in values.iter().enumerate() {
                let k = (i / width / 2) * width.div_ceil(2) + i % width / 2;
                parent[k] = parent[k].min(v);
            }
            levels.push((width, vec![0; values.len()], values));
            if width == 1 && height == 1 {
                break;
            }
            width = width.div_ceil(2);
            height = height.div_ceil(2);
            values = parent;
        }
        Self { levels }
    }
    // At threshold one each inclusion node emits exactly one bit, on its
    // first raster-order visit. The zero-plane minima already tell us whether
    // its subtree contains any selected contribution. An empty node suppresses
    // all descendant bits, including the rest of this row inside the node.
    fn emit_inclusion(&self, x: usize, y: usize, out: &mut Writer) -> usize {
        for (shift, (stride, _, minima)) in self.levels.iter().enumerate().rev() {
            let side = 1usize << shift;
            let present = minima[(y >> shift) * stride + (x >> shift)] != u32::MAX;
            if (x | y) & (side - 1) == 0 {
                out.bit(u8::from(present));
            }
            if !present {
                return side - (x & (side - 1));
            }
        }
        1
    }
    fn emit(&mut self, x: usize, y: usize, threshold: u32, out: &mut Writer) {
        let mut inherited = 0;
        for (shift, (width, states, values)) in self.levels.iter_mut().enumerate().rev() {
            let i = (y >> shift) * *width + (x >> shift);
            let value = values[i];
            let low = &mut states[i];
            *low = (*low).max(inherited);
            while *low < threshold && *low <= value {
                out.bit(u8::from(*low == value));
                *low += 1;
            }
            inherited = (*low).min(value);
        }
    }
}

#[derive(Debug)]
struct Contribution {
    zeros: u32,
    passes: u32,
    body: Range<u64>,
    fields: u128,
    bits: u8,
}
enum Body {
    Range(Range<u64>),
    Cached(Range<u64>, Arc<[u8]>),
}
impl Body {
    fn len(&self) -> u64 {
        match self {
            Self::Range(r) => r.end - r.start,
            Self::Cached(_, bytes) => bytes.len() as u64,
        }
    }
}
struct SparsePacket {
    header: Vec<u8>,
    ranges: Vec<Body>,
}

fn position(bits: &Bits<'_>) -> (usize, u8) {
    (bits.offset, bits.remaining)
}

#[derive(Debug, Default)]
pub(super) struct Index {
    packets: HashMap<u64, Arc<PacketIndex>>,
    pub(super) bytes: usize,
    indexed: bool,
}
#[derive(Debug)]
struct PacketIndex {
    body: u64,
    bands: Vec<(usize, usize, Vec<Option<Contribution>>)>,
}

fn parse_packet(
    h: &Header,
    packet: &Packet,
    bytes: &[u8],
    extent: Range<u64>,
) -> Result<PacketIndex> {
    let grids = prefix_codeblocks(h, 0, packet)?.context("missing codeblock geometry")?;
    let mut input = Bits {
        bytes,
        offset: 0,
        remaining: 0,
        byte: 0,
        previous: 0,
    };
    let mut body_len = 0;
    let mut bands = Vec::new();
    if input.bit()? != 0 {
        for (width, height) in grids {
            let mut inclusion = TagTree::new(width, height);
            let mut planes = TagTree::new(width, height);
            let mut blocks = Vec::with_capacity(width * height);
            for i in 0..width * height {
                if !inclusion.decode(i, 1, &mut input)? {
                    blocks.push(None);
                    continue;
                }
                let mut zeros = 0;
                while !planes.decode(i, zeros + 1, &mut input)? {
                    zeros += 1;
                    ensure!(zeros < 32, "invalid zero-plane count");
                }
                let mut field_start = input.clone();
                let passes = input.coding_passes()?;
                ensure!(passes <= 109, "multiple codeword segments");
                let mut lblock = 3;
                while input.bit()? != 0 {
                    lblock += 1;
                    ensure!(lblock <= 31, "invalid codeblock length");
                }
                let count = lblock + passes.ilog2();
                ensure!(count <= 32, "invalid segment length");
                let length = input.bits(count)?;
                let end = body_len + length;
                ensure!(end <= extent.end - extent.start, "codeblock exceeds packet");
                let mut fields = 0u128;
                let mut bits = 0;
                while position(&field_start) != position(&input) {
                    fields = fields << 1 | u128::from(field_start.bit()?);
                    bits += 1;
                }
                blocks.push(Some(Contribution {
                    zeros,
                    passes: passes as u32,
                    body: body_len..end,
                    fields,
                    bits,
                }));
                body_len = end;
            }
            bands.push((width, height, blocks));
        }
    }
    let body = extent.start + input.offset as u64 + u64::from(input.previous == 255);
    ensure!(
        body + body_len == extent.end,
        "packet header and PLT disagree"
    );
    Ok(PacketIndex { body, bands })
}

impl PacketIndex {
    fn select(
        &self,
        h: &Header,
        packet: &Packet,
        rect: [u32; 4],
        namespace: [u8; 32],
        grok: Option<bool>,
        planes: &[u32],
        jobs: &mut Vec<arrival::Word>,
        metadata_left: &mut usize,
        block_size: u64,
    ) -> SparsePacket {
        let mut output = Writer::default();
        let mut ranges: Vec<Body> = Vec::new();
        output.bit(u8::from(!self.bands.is_empty()));
        for (index, (width, height, all)) in self.bands.iter().enumerate() {
            let (width, height) = (*width, *height);
            let r = packet.resolution;
            let band = if r == 0 { 0 } else { index + 1 };
            let shift = h.levels - 1 - r + u8::from(r > 0);
            let scale = 1u32 << shift;
            let project = |coord: u32, high: usize| {
                coord
                    .saturating_sub(high as u32 * (scale / 2))
                    .div_ceil(scale)
            };
            // Two subband samples on both sides cover OpenJPEG's reversible
            // 5/3 synthesis support at this resolution.
            let x0 = project(rect[0], band & 1).saturating_sub(2);
            let x1 = project(rect[2], band & 1) + 2;
            let y0 = project(rect[1], band >> 1).saturating_sub(2);
            let y1 = project(rect[3], band >> 1) + 2;
            let cw = 1u32 << (h.cod[6] + 2);
            let ch = 1u32 << (h.cod[7] + 2);
            let blocks: Vec<_> = all
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let x = (i % width) as u32 * cw;
                    let y = (i / width) as u32 * ch;
                    b.as_ref()
                        .filter(|_| x < x1 && y < y1 && x + cw > x0 && y + ch > y0)
                })
                .collect();
            let mut zbp = EncoderTree::new(
                width,
                height,
                blocks
                    .iter()
                    .map(|b| b.as_ref().map_or(u32::MAX, |b| b.zeros))
                    .collect(),
            );
            let mut cursor = 0;
            while cursor < blocks.len() {
                let i = cursor;
                let x = i % width;
                cursor += zbp.emit_inclusion(x, i / width, &mut output).min(width - x);
                if let Some(b) = blocks[i] {
                    zbp.emit(i % width, i / width, b.zeros + 1, &mut output);
                    for bit in (0..b.bits).rev() {
                        output.bit((b.fields >> bit) as u8 & 1);
                    }
                    if !b.body.is_empty() {
                        let span = self.body + b.body.start..self.body + b.body.end;
                        let grid = [(i % width) as u32, (i / width) as u32];
                        let (x, y) = (grid[0] * cw, grid[1] * ch);
                        let w = cw.min(project(h.width, band & 1).saturating_sub(x));
                        let height = ch.min(project(h.height, band >> 1).saturating_sub(y));
                        let key = [
                            u32::from(packet.component),
                            u32::from(r),
                            band as u32,
                            x,
                            y,
                            w,
                            height,
                            0,
                            b.passes,
                            (span.end - span.start) as u32,
                        ];
                        if let Some(word) = grok.and_then(|driver| {
                            crate::decoder::block_reuse::planned_word(namespace, driver, key, grid)
                        }) {
                            ranges.push(Body::Cached(span, word));
                        } else {
                            let band_index = usize::from(r.saturating_sub(1)) * 3 + band;
                            // Account for the word, its readiness edges and the dispatcher's
                            // block map. Other contributions remain on the ordinary codec path.
                            let pages = span.end.div_ceil(block_size) - span.start / block_size;
                            let charge =
                                192usize.saturating_add((pages as usize).saturating_mul(128));
                            if let Some(&planes) = planes
                                .get(band_index)
                                .filter(|_| charge <= *metadata_left && key[9] <= 65536)
                            {
                                *metadata_left -= charge;
                                jobs.push(arrival::Word {
                                    span: span.clone(),
                                    key,
                                    grid,
                                    planes,
                                    zeros: b.zeros,
                                });
                            }
                            if let Some(Body::Range(previous)) = ranges
                                .last_mut()
                                .filter(|r| matches!(r, Body::Range(r) if r.end == span.start))
                            {
                                previous.end = span.end;
                            } else {
                                ranges.push(Body::Range(span));
                            }
                        }
                    }
                }
            }
        }
        SparsePacket {
            header: output.finish(),
            ranges,
        }
    }
}

impl RemoteSource {
    pub(crate) fn warm_codeblock_index(&self) -> Result<()> {
        let Ok(_owner) = self.index_warmup.try_lock() else {
            return Ok(());
        };
        if self
            .codeblocks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .indexed
        {
            return Ok(());
        }
        let mut context = ReadContext::new(None);
        if context.explicit_request_limit {
            return Ok(());
        }
        let payload = self.acquire_render_header(&mut context)?;
        self.codeblock_view(&payload, &[(0, 0)], 1, 0, true, &mut context)?;
        Ok(())
    }
    pub(super) fn codeblock_view(
        &self,
        payload: &HeaderPayload,
        tiles: &[(u32, u32)],
        edge: u32,
        reduction: u8,
        index_only: bool,
        context: &mut ReadContext,
    ) -> Result<Option<PreparedSubset>> {
        let h = &self.header;
        // One origin-aligned native tile, one precinct per band, one layer,
        // one segment per codeblock. Other layouts use the existing planner.
        if !h.packet_compatible
            || h.is_ht
            || h.layers != 1
            || h.cod[0] != 0
            || h.cod[8] != 0
            || h.cod[9] != 1
            || h.image_origin != (0, 0)
            || (h.layout.origin_x, h.layout.origin_y) != (0, 0)
            || h.layout.columns != 1
            || h.layout.rows != 1
            || h.width > 32768
            || h.height > 32768
            || h.width < 512
            || h.height < 512
            || self.meta.size < 8 << 20
            || tiles.is_empty()
        {
            return Ok(None);
        }
        let mut rect = [h.width, h.height, 0, 0];
        for &(x, y) in tiles {
            let x = x.saturating_mul(edge);
            let y = y.saturating_mul(edge);
            rect = [
                rect[0].min(x),
                rect[1].min(y),
                rect[2].max(x.saturating_add(edge).min(h.width)),
                rect[3].max(y.saturating_add(edge).min(h.height)),
            ];
        }
        if rect[0] >= rect[2]
            || rect[1] >= rect[3]
            || u64::from(rect[2] - rect[0]) * u64::from(rect[3] - rect[1]) * 4
                > u64::from(h.width) * u64::from(h.height)
        {
            return Ok(None);
        }
        let part = self.part_at(h.first_part, context)?;
        if part.tile != 0
            || part.ordinal != 0
            || part.total > 1
            || part.offset + part.length != h.codestream_end - 2
        {
            return Ok(None);
        }
        let (markers, start, lengths, compatible) = self.tile_header(&part, context)?;
        let Some(lengths) = lengths.filter(|_| compatible) else {
            return Ok(None);
        };
        let (order, _) = self.ordered_packets(0, None, context)?;
        ensure!(lengths.len() == order.len(), "invalid PLT packet count");
        // A bounded reduced packet set also seeds the next zoom level. At this
        // scale many scattered codeblock reads cost more across parent and child.
        // Native Grok can render a reduced ROI cheaply from an existing index.
        // OpenJPEG benefits more from the contiguous packets across parent and child.
        let indexed_grok_parent = reduction > 0 && context.planning_driver_grok == Some(true)
            && self.codeblocks.lock().unwrap_or_else(|e| e.into_inner()).indexed;
        if reduction > 0 && !index_only && !indexed_grok_parent {
            let packet_bytes = order
                .iter()
                .zip(lengths.iter())
                .filter(|(packet, _)| packet.resolution < h.levels - reduction)
                .try_fold(0u64, |sum, (_, &len)| sum.checked_add(len));
            let reusable = context
                .limit
                .saturating_sub(context.stats.bytes)
                .min(self.cache.effective_limit() as u64)
                .min(16 << 20);
            if packet_bytes.is_some_and(|bytes| bytes <= reusable) {
                return Ok(None);
            }
        }
        let mut total_blocks = 0;
        let mut cursor = start;
        let mut extents = Vec::new();
        let mut probes = Vec::new();
        for (p, &len) in order.iter().zip(lengths.iter()) {
            let Some(grids) = prefix_codeblocks(h, 0, p)? else {
                return Ok(None);
            };
            if grids.len() != if p.resolution == 0 { 1 } else { 3 } {
                return Ok(None);
            }
            let blocks: usize = grids.iter().map(|(w, h)| w * h).sum();
            total_blocks += blocks;
            if total_blocks > 128 * 1024 {
                return Ok(None);
            }
            let end = cursor.checked_add(len).context("packet overflow")?;
            ensure!(
                len > 0 && end <= part.offset + part.length,
                "invalid PLT extent"
            );
            extents.push(cursor..end);
            probes.push((true, cursor..end.min(cursor + 64 + blocks as u64 * 6)));
            cursor = end;
        }
        ensure!(cursor == part.offset + part.length, "invalid PLT coverage");
        let cached = {
            let index = self.codeblocks.lock().unwrap_or_else(|e| e.into_inner());
            extents
                .iter()
                .map(|r| index.packets.get(&r.start).cloned())
                .collect::<Vec<_>>()
        };
        let needed: Vec<_> = probes
            .iter()
            .zip(&cached)
            .zip(order.iter())
            .filter(|((_, entry), p)| entry.is_none() && p.resolution < h.levels - reduction)
            .map(|((probe, _), _)| probe.clone())
            .collect();
        // Header transfers and payload reads share the physical-attempt budget. Reserve
        // aligned header bytes up front so simultaneous batches cannot overspend.
        let header_bytes = needed
            .iter()
            .map(|(_, r)| {
                (r.end.div_ceil(self.block_size) - r.start / self.block_size) * self.block_size
            })
            .sum::<u64>();
        let remaining = context.limit.saturating_sub(context.stats.bytes);
        if header_bytes >= remaining {
            return Ok(None);
        }
        let mut headers = ReadContext::new(context.cancel.clone());
        headers.deadline = context.deadline;
        headers.wire_budget = context.wire_budget.clone();
        headers.requests_limit = context.requests_limit;
        headers.feedback = context.feedback.clone();
        headers.trace_id = context.trace_id;
        headers.limit = header_bytes;
        let (tx, rx) = std::sync::mpsc::channel();
        headers.ready_blocks = Some(tx);
        let previous_limit = context.limit;
        context.limit -= header_bytes;
        let planes = if !index_only && reduction == 0 && context.tier1_threads > 1 {
            arrival::planes(&payload.main, &markers, h.levels).unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut tier1 = Vec::new();
        let metadata_available = context
            .metadata_limit
            .saturating_sub(context.metadata_bytes);
        let mut metadata_left = metadata_available;
        let mut packets: Vec<_> = order.iter().map(|_| None).collect();
        let mut blocks = HashMap::new();
        let mut header_blocks = HashMap::new();
        let mut output_size = 0;
        let assembled = std::thread::scope(|scope| -> Result<bool> {
            let worker = if needed.is_empty() {
                None
            } else {
                Some(scope.spawn(|| {
                    let result = self.prefetch_ranges(&needed, &mut headers, usize::MAX);
                    headers.ready_blocks.take();
                    result
                }))
            };
            let mut early = None;
            let mut required = Vec::new();
            let produce = (|| -> Result<bool> {
                let mut pending: Vec<_> = (0..order.len()).collect();
                while !pending.is_empty() {
                    context.check()?;
                    while let Ok(arrived) = rx.try_recv() {
                        header_blocks.extend(arrived);
                    }
                    let mut spans = Vec::new();
                    let mut wait = Vec::new();
                    for i in pending {
                        let packet = &order[i];
                        if packet.resolution >= h.levels - reduction {
                            packets[i] = Some(SparsePacket {
                                header: vec![0],
                                ranges: Vec::new(),
                            });
                            output_size += 1;
                            continue;
                        }
                        let entry = if let Some(entry) = &cached[i] {
                            entry.clone()
                        } else {
                            let probe = &probes[i].1;
                            if !(probe.start / self.block_size..probe.end.div_ceil(self.block_size))
                                .all(|b| header_blocks.contains_key(&(b * self.block_size)))
                            {
                                wait.push(i);
                                continue;
                            }
                            let mut data = Vec::new();
                            copy_range(&header_blocks, self.block_size, probe.clone(), &mut data)?;
                            let offset = extents[i].start;
                            let Ok(parsed) = parse_packet(h, packet, &data, extents[i].clone())
                            else {
                                return Ok(false);
                            };
                            let bytes = 128
                                + parsed
                                    .bands
                                    .iter()
                                    .map(|(_, _, v)| {
                                        v.capacity() * std::mem::size_of::<Option<Contribution>>()
                                            + 64
                                    })
                                    .sum::<usize>();
                            let entry = Arc::new(parsed);
                            let mut index =
                                self.codeblocks.lock().unwrap_or_else(|e| e.into_inner());
                            if !index.packets.contains_key(&offset)
                                && index.bytes + bytes <= context.metadata_limit
                            {
                                index.packets.insert(offset, entry.clone());
                                index.bytes += bytes;
                                self.metadata_cost.set_codeblock_bytes(index.bytes);
                            }
                            entry
                        };
                        if index_only {
                            continue;
                        }
                        let selected = entry.select(
                            h,
                            packet,
                            rect,
                            self.block_namespace,
                            context.planning_driver_grok,
                            &planes,
                            &mut tier1,
                            &mut metadata_left,
                            self.block_size,
                        );
                        output_size += selected.header.len() as u64
                            + selected.ranges.iter().map(Body::len).sum::<u64>();
                        ensure!(
                            output_size <= previous_limit,
                            "compressed subset exceeds request memory budget"
                        );
                        spans.extend(selected.ranges.iter().filter_map(|body| match body {
                            Body::Range(r) => Some((true, r.clone())),
                            Body::Cached(..) => None,
                        }));
                        packets[i] = Some(selected);
                    }
                    // Start an independent useful read while the remaining probes arrive.
                    // Keep the rest of the demand together: replanning each arrival fragments
                    // contiguous reads and can add a full RTT per group of completed headers.
                    if early.is_none() && !wait.is_empty() {
                        let allowance = context
                            .limit
                            .saturating_sub(context.stats.bytes)
                            .min(512 * 1024);
                        let candidate = spans
                            .iter()
                            .map(|(_, r)| r)
                            .filter(|r| {
                                r.end - r.start >= self.block_size
                                    && r.end - r.start + 2 * self.block_size <= allowance
                                    && !self.cache.contains(
                                        &self.cache_key,
                                        self.block_size,
                                        r.start / self.block_size * self.block_size,
                                    )
                            })
                            .max_by_key(|r| r.end - r.start)
                            .cloned();
                        if let Some(range) = candidate {
                            let mut first = ReadContext::new(context.cancel.clone());
                            first.deadline = context.deadline;
                            first.limit = allowance;
                            first.requests_limit = context.requests_limit;
                            first.wire_budget = context.wire_budget.clone();
                            first.feedback = context.feedback.clone();
                            first.trace_id = context.trace_id;
                            early = Some(scope.spawn(move || {
                                let result =
                                    self.prefetch_ranges(&[(true, range)], &mut first, usize::MAX);
                                (result, first.stats)
                            }));
                        }
                    }
                    required.extend(spans);
                    pending = wait;
                    if !pending.is_empty() {
                        match rx.recv_timeout(
                            context.deadline.saturating_duration_since(Instant::now()),
                        ) {
                            Ok(arrived) => header_blocks.extend(arrived),
                            Err(_) => anyhow::bail!(
                                "packet header stream ended before all probes arrived"
                            ),
                        }
                    }
                }
                Ok(true)
            })();
            let header_result = worker.map(|worker| worker.join());

            if let Some(read) = early {
                let (result, stats) = read
                    .join()
                    .map_err(|_| anyhow::anyhow!("early packet read stopped"))?;
                context.stats.bytes += stats.bytes;
                context.stats.cache_bytes += stats.cache_bytes;
                context.stats.requests += stats.requests;
                context.pin_packet_blocks(result?);
            }
            if let Some(result) = header_result {
                result.map_err(|_| anyhow::anyhow!("packet header worker stopped"))??;
            }
            if !produce? {
                return Ok(false);
            }
            if !required.is_empty() {
                let charge = metadata_available - metadata_left;
                if context.admit_metadata(charge) {
                    let fetched = arrival::prefetch(self, &required, context, tier1);
                    context.metadata_bytes -= charge;
                    blocks = fetched?;
                } else {
                    blocks = self.prefetch_codeblocks(&required, context)?;
                }
            }
            Ok(true)
        });
        context.limit = previous_limit;
        context.stats.bytes += headers.stats.bytes;
        context.stats.requests += headers.stats.requests;
        context.stats.cache_bytes += headers.stats.cache_bytes;
        if !assembled? {
            return Ok(None);
        }
        if index_only {
            self.codeblocks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .indexed = true;
            return Ok(None);
        }
        let compact = if reduction > 0
            && h.layout.width % (1 << reduction) == 0
            && h.layout.height % (1 << reduction) == 0
        {
            resolution_view::project(&payload.main[2..], h, reduction, true)?
                .zip(resolution_view::project(&markers, h, reduction, false)?)
        } else {
            None
        };
        let projected = compact.is_some();
        let (mut stream, markers) = if let Some((main, markers)) = compact {
            let mut stream = vec![0xff, 0x4f];
            stream.extend(main);
            (stream, markers)
        } else {
            (payload.main.clone(), markers)
        };

        let sot = stream.len();
        stream.extend(self.read(part.offset..part.offset + 12, context)?);
        stream.extend(markers);
        stream.extend([0xff, 0x93]);
        let mut cached_words = 0usize;
        let mut cached_bytes = 0usize;
        let mut uncached_bytes = 0u64;
        // Detailed traces are opt-in. Offsets let the offline benchmark verify
        // cached words against the immutable original, independently of either codec.
        let mut audit = std::env::var("RIVER_PROFILE_CODEWORD_RANGES")
            .is_ok_and(|v| v == "1")
            .then(|| (Vec::new(), Vec::new(), sha2::Sha256::new()));
        use sha2::Digest;
        for (packet, position) in packets.into_iter().zip(order.iter()) {
            if projected && position.resolution >= h.levels - reduction {
                continue;
            }
            let packet = packet.context("missing selected packet")?;
            stream.extend(packet.header);
            for range in packet.ranges {
                match range {
                    Body::Range(r) => {
                        if let Some((want, _, _)) = &mut audit {
                            want.push((r.start, r.end));
                        }
                        uncached_bytes += r.end - r.start;
                        copy_range(&blocks, self.block_size, r, &mut stream)?;
                    }
                    Body::Cached(r, word) => {
                        if let Some((_, hits, hash)) = &mut audit {
                            hits.push((r.start, r.end));
                            hash.update(&word);
                        }
                        cached_words += 1;
                        cached_bytes += word.len();
                        stream.extend_from_slice(&word);
                    }
                }
            }
        }
        if let Some((want, hits, hash)) = audit {
            tracing::info!(want=?want,hits=?hits,sha256=%hex::encode(hash.finalize()),"codeword range audit");
        }
        if std::env::var_os("RIVER_PROFILE_SOURCE").is_some() {
            tracing::info!(
                cached_words,
                cached_bytes,
                uncached_bytes,
                "planned codeblock payload"
            );
        }
        let length = u32::try_from(stream.len() - sot)?;
        stream[sot + 6..sot + 10].copy_from_slice(&length.to_be_bytes());
        stream.extend([0xff, 0xd9]);
        let jp2 = !payload.container.is_empty();
        let bytes = if jp2 {
            let mut b = payload.container.clone();
            if projected {
                resize_jp2(
                    &mut b,
                    h.width.div_ceil(1 << reduction),
                    h.height.div_ceil(1 << reduction),
                )?;
            }
            b.extend(u32::try_from(stream.len() + 8)?.to_be_bytes());
            b.extend(b"jp2c");
            b.extend(stream);
            b
        } else {
            stream
        };
        Ok(Some(PreparedSubset {
            view_reduction: if projected { reduction } else { 0 },
            technical_origin: None,
            bytes: bytes.into(),
            route: "s3-codeblock-ranges",
            jp2,
            width: h.width,
            height: h.height,
            layout: h.layout,
            offset_x: 0,
            offset_y: 0,
        }))
    }
}

fn copy_range(
    blocks: &HashMap<u64, Arc<[u8]>>,
    size: u64,
    range: Range<u64>,
    out: &mut Vec<u8>,
) -> Result<()> {
    let mut pos = range.start;
    while pos < range.end {
        let offset = pos / size * size;
        let bytes = blocks
            .get(&offset)
            .context("missing retained codeblock range")?;
        let within = (pos - offset) as usize;
        let n = (range.end - pos).min(bytes.len().saturating_sub(within) as u64) as usize;
        ensure!(n > 0, "truncated codeblock range");
        out.extend_from_slice(&bytes[within..within + n]);
        pos += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewritten_tag_tree_round_trips_odd_grids_and_absent_blocks() {
        let values = [0, 2, 6, 32, 2, 1, 5, 31, 0, 3, 18, 32, 4, 32, 1];
        let mut tree = EncoderTree::new(5, 3, values.to_vec());
        let mut writer = Writer::default();
        for (i, &v) in values.iter().enumerate() {
            tree.emit(i % 5, i / 5, (v + 1).min(32), &mut writer);
        }
        let bytes = writer.finish();
        let mut input = Bits {
            bytes: &bytes,
            offset: 0,
            remaining: 0,
            byte: 0,
            previous: 0,
        };
        let mut decoded = TagTree::new(5, 3);
        for (i, &v) in values.iter().enumerate() {
            for threshold in 1..=(v + 1).min(32) {
                assert_eq!(
                    decoded.decode(i, threshold, &mut input).unwrap(),
                    v < threshold
                );
            }
        }
    }

    #[test]
    fn sparse_inclusion_is_byte_identical_to_the_full_threshold_one_tree() {
        for (w, h) in [(1, 1), (3, 3), (5, 2), (17, 9)] {
            for seed in 0..512usize {
                let values: Vec<_> = (0..w*h).map(|i| {
                    if seed & (1 << ((i + i/9) % 9)) != 0 { (i % 32) as u32 } else { u32::MAX }
                }).collect();
                let mut reference = EncoderTree::new(w, h, values.iter()
                    .map(|v| if *v == u32::MAX { u32::MAX } else { 0 }).collect());
                let sparse = EncoderTree::new(w, h, values);
                let (mut expected, mut actual) = (Writer::default(), Writer::default());
                for y in 0..h {
                    for x in 0..w { reference.emit(x, y, 1, &mut expected); }
                    let mut x = 0;
                    while x < w { x += sparse.emit_inclusion(x, y, &mut actual); }
                }
                assert_eq!(expected.remaining, actual.remaining);
                assert_eq!(expected.finish(), actual.finish());
            }
        }
    }

    #[test]
    fn header_stuffing_reserves_the_bit_after_ff() {
        let mut writer = Writer::default();
        for _ in 0..15 {
            writer.bit(1);
        }
        assert_eq!(writer.finish(), vec![255, 127]);
        let mut writer = Writer::default();
        for _ in 0..8 {
            writer.bit(1);
        }
        assert_eq!(writer.finish(), vec![255, 0]);
    }
}
