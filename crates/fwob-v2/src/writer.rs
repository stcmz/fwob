use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    path::Path,
};

use fwob_core::{FrameRef, FwobError, Key, KeyType, Schema};

use crate::{
    encoding::{decode_page_payload, encode_page_payload},
    file_header::{
        update_counts, write_file_header, FileHeader, FILE_HEADER_LEN, MAX_PAGE_SIZE, MIN_PAGE_SIZE,
    },
    page::{Encoding, PageHeader, PAGE_HEADER_LEN},
    Codec, Result, V2Error,
};

const INTERPOLATED_PROBE_WINDOW_MARGIN_DIVISOR: usize = 0;
const RECORDED_WINDOW_SPANS: usize = 10;
const INITIAL_COMPRESSED_PROBE_RAW_PAGES: usize = 4;
/// Maximum number of trailing uncompressed pages a compressing codec will reclaim and repack
/// when opening a file for append. Bounds the work: a file with thousands of raw pages reclaims
/// at most this many. `Codec::None` only ever coalesces the single trailing page.
const MAX_APPEND_TAIL_PAGES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecSelection {
    Fixed(Codec),
    Smallest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodingSelection {
    Fixed(Encoding),
    Smallest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagePacking {
    EstimateShrink,
    TightFit,
}

pub const DEFAULT_PAGE_SIZE: u32 = 512 * 1024;
pub const DEFAULT_CODEC: Codec = Codec::Zstd;
pub const DEFAULT_ZSTD_LEVEL: i32 = 6;
pub const DEFAULT_ENCODING: Encoding = Encoding::ColumnarBasicV1;
pub const DEFAULT_PAGE_PACKING: PagePacking = PagePacking::EstimateShrink;

#[derive(Debug, Clone, Copy, Default)]
pub struct PackingStats {
    pub first_page_attempts: u64,
    pub subsequent_page_attempts: u64,
    pub subsequent_pages: u64,
    pub subsequent_min_attempts: u64,
    pub subsequent_max_attempts: u64,
    pub initial_window_attempts: u64,
    pub window_frame_spans: [u64; RECORDED_WINDOW_SPANS],
    pub window_span_counts: [u64; RECORDED_WINDOW_SPANS],
    pub initial_windows: u64,
    pub window_final_position_sums: [f64; RECORDED_WINDOW_SPANS],
    pub window_final_position_counts: [u64; RECORDED_WINDOW_SPANS],
}

impl PackingStats {
    pub fn subsequent_average_attempts(self) -> f64 {
        if self.subsequent_pages == 0 {
            0.0
        } else {
            self.subsequent_page_attempts as f64 / self.subsequent_pages as f64
        }
    }

    pub fn average_initial_window_attempts(self) -> f64 {
        if self.initial_windows == 0 {
            0.0
        } else {
            self.initial_window_attempts as f64 / self.initial_windows as f64
        }
    }

    pub fn average_window_frame_span(self, index: usize) -> f64 {
        if index >= RECORDED_WINDOW_SPANS || self.window_span_counts[index] == 0 {
            0.0
        } else {
            self.window_frame_spans[index] as f64 / self.window_span_counts[index] as f64
        }
    }

    pub fn average_window_final_position(self, index: usize) -> f64 {
        if index >= RECORDED_WINDOW_SPANS || self.window_final_position_counts[index] == 0 {
            0.0
        } else {
            self.window_final_position_sums[index] / self.window_final_position_counts[index] as f64
        }
    }
}

#[derive(Debug, Clone)]
pub struct WriterOptions {
    pub title: String,
    pub page_size: u32,
    pub codec: Codec,
    pub codec_selection: CodecSelection,
    pub zstd_level: i32,
    pub encoding: Encoding,
    pub encoding_selection: EncodingSelection,
    pub string_table: Vec<String>,
    pub compress_partial_page: bool,
    pub page_packing: PagePacking,
}

impl WriterOptions {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            page_size: DEFAULT_PAGE_SIZE,
            codec: DEFAULT_CODEC,
            codec_selection: CodecSelection::Fixed(DEFAULT_CODEC),
            zstd_level: DEFAULT_ZSTD_LEVEL,
            encoding: DEFAULT_ENCODING,
            encoding_selection: EncodingSelection::Fixed(DEFAULT_ENCODING),
            string_table: Vec::new(),
            compress_partial_page: false,
            page_packing: DEFAULT_PAGE_PACKING,
        }
    }
}

pub struct Writer<W> {
    inner: W,
    header: FileHeader,
    key_type: KeyType,
    options: WriterOptions,
    pending: PendingFrames,
    last_key: Option<Key>,
    next_compaction_frame_count: usize,
    compressed_page_frame_hint: usize,
    zstd_compressor: Option<zstd::bulk::Compressor<'static>>,
    packing_stats: PackingStats,
    current_page_attempts: u64,
    append_tail: Option<AppendTail>,
}

type CompressedCandidate = (Codec, Encoding, Vec<u8>, usize);
type FittingCandidate = (usize, Codec, Encoding, Vec<u8>, usize);

pub trait Resize {
    fn resize_len(&mut self, len: u64) -> std::io::Result<()>;
}

impl Resize for File {
    fn resize_len(&mut self, len: u64) -> std::io::Result<()> {
        self.set_len(len)
    }
}

impl Resize for Cursor<Vec<u8>> {
    fn resize_len(&mut self, len: u64) -> std::io::Result<()> {
        self.get_mut().resize(len as usize, 0);
        if self.position() > len {
            self.set_position(len);
        }
        Ok(())
    }
}

impl<T: Resize + ?Sized> Resize for &mut T {
    fn resize_len(&mut self, len: u64) -> std::io::Result<()> {
        (**self).resize_len(len)
    }
}

struct AppendTail {
    /// First page of the whole reclaimable tail (full raw pages included). Used by the
    /// compression path, which may recompress the entire run.
    start_page: u64,
    page_count: u64,
    frame_count: u64,
    /// First page of the trailing run of under-filled pages (`>= start_page`). A raw flush only
    /// ever rewrites pages from here on, leaving the dense leading pages in place.
    underfilled_start_page: u64,
    underfilled_frame_count: u64,
    loaded: bool,
}

struct PendingFrames {
    segments: VecDeque<Vec<u8>>,
    head_offset: usize,
    len: usize,
    scratch: Vec<u8>,
}

impl PendingFrames {
    fn new() -> Self {
        Self {
            segments: VecDeque::new(),
            head_offset: 0,
            len: 0,
            scratch: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn frame_count(&self, frame_len: usize) -> usize {
        self.len / frame_len
    }

    fn byte_len(&self) -> usize {
        self.len
    }

    fn append_copy(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.segments.push_back(bytes.to_vec());
        self.len += bytes.len();
    }

    fn append_owned(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.len += bytes.len();
        self.segments.push_back(bytes);
    }

    fn consume_front(&mut self, mut count: usize) {
        debug_assert!(count <= self.len);
        self.len -= count;
        while count > 0 {
            let front_len = self
                .segments
                .front()
                .map(|front| front.len() - self.head_offset)
                .unwrap_or(0);
            if count < front_len {
                self.head_offset += count;
                return;
            }

            count -= front_len;
            self.segments.pop_front();
            self.head_offset = 0;
        }
    }

    fn prefix_contiguous(&mut self, count: usize) -> &[u8] {
        debug_assert!(count <= self.len);
        if count == 0 {
            return &[];
        }
        if let Some(front) = self.segments.front() {
            let available = front.len() - self.head_offset;
            if count <= available {
                return &front[self.head_offset..self.head_offset + count];
            }
        }

        self.scratch.clear();
        self.scratch.reserve(count);
        let mut remaining = count;
        let mut first = true;
        for segment in &self.segments {
            let start = if first { self.head_offset } else { 0 };
            first = false;
            let available = segment.len() - start;
            let take = remaining.min(available);
            self.scratch
                .extend_from_slice(&segment[start..start + take]);
            remaining -= take;
            if remaining == 0 {
                break;
            }
        }
        &self.scratch
    }

    fn copy_range(&self, offset: usize, len: usize) -> Vec<u8> {
        debug_assert!(offset + len <= self.len);
        let mut out = Vec::with_capacity(len);
        let mut skip = offset;
        let mut remaining = len;
        let mut first = true;
        for segment in &self.segments {
            let start = if first { self.head_offset } else { 0 };
            first = false;
            let available = segment.len() - start;
            if skip >= available {
                skip -= available;
                continue;
            }
            let segment_start = start + skip;
            let take = remaining.min(segment.len() - segment_start);
            out.extend_from_slice(&segment[segment_start..segment_start + take]);
            remaining -= take;
            skip = 0;
            if remaining == 0 {
                break;
            }
        }
        out
    }
}

impl Writer<File> {
    pub fn create(path: impl AsRef<Path>, schema: Schema, options: WriterOptions) -> Result<Self> {
        let file = File::create(path)?;
        Self::new(file, schema, options)
    }

    pub fn open_append(path: impl AsRef<Path>, options: WriterOptions) -> Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        let header = crate::file_header::read_file_header(&mut file)?;
        let key_type = KeyType::from_field(header.schema.key_field())?;

        // Identify the trailing run of uncompressed pages that can be reclaimed on append, and
        // within it the trailing run of *under-filled* pages. Two boundaries are tracked:
        //   - `tail_start`: start of the whole reclaimable tail (full raw pages included). A
        //     compressing codec may recompress this entire run, up to MAX_APPEND_TAIL_PAGES,
        //     stopping only at the first compressed page.
        //   - `underfilled_start`: start of the trailing run of under-filled pages. A plain raw
        //     flush (no full compressed page produced) only ever rewrites this run, so already
        //     dense pages are never touched.
        // `Codec::None` never compresses, so its tail is just the single trailing under-filled
        // page ("append to the last page until full").
        let raw_page_capacity = ((header.page_size as usize - PAGE_HEADER_LEN)
            / header.schema.frame_len as usize)
            .max(1);
        let max_tail_pages = if options.codec == Codec::None {
            1
        } else {
            MAX_APPEND_TAIL_PAGES
        };
        let min_tail_start = header.page_count.saturating_sub(max_tail_pages as u64);
        let mut tail_start = header.page_count;
        let mut underfilled_start = header.page_count;
        let mut run_open = true;
        while tail_start > min_tail_start {
            file.seek(SeekFrom::Start(header.page_offset(tail_start - 1)))?;
            let page = PageHeader::read(&mut file, tail_start - 1)?;
            if page.codec != Codec::None {
                break; // compressed page: cannot grow / repack across it
            }
            if run_open && page.frame_count as usize >= raw_page_capacity {
                run_open = false; // first full page (from the end) closes the under-filled run
            }
            if options.codec == Codec::None && !run_open {
                break; // None reclaims only the trailing under-filled page, never a full one
            }
            tail_start -= 1;
            if run_open {
                underfilled_start = tail_start;
            }
        }

        let mut tail_frames = 0u64;
        let mut underfilled_frames = 0u64;
        let mut last_key = if tail_start > 0 {
            file.seek(SeekFrom::Start(header.page_offset(tail_start - 1)))?;
            Some(PageHeader::read(&mut file, tail_start - 1)?.last_key)
        } else {
            None
        };
        for page_index in tail_start..header.page_count {
            file.seek(SeekFrom::Start(header.page_offset(page_index)))?;
            let page = PageHeader::read(&mut file, page_index)?;
            tail_frames += u64::from(page.frame_count);
            if page_index >= underfilled_start {
                underfilled_frames += u64::from(page.frame_count);
            }
            last_key = Some(page.last_key);
        }

        let append_tail = if tail_frames == 0 {
            None
        } else {
            Some(AppendTail {
                start_page: tail_start,
                page_count: header.page_count - tail_start,
                frame_count: tail_frames,
                underfilled_start_page: underfilled_start,
                underfilled_frame_count: underfilled_frames,
                loaded: false,
            })
        };
        let append_offset = FILE_HEADER_LEN + header.page_count * u64::from(header.page_size);
        file.seek(SeekFrom::Start(append_offset))?;

        let mut append_options = options;
        append_options.title = header.title.clone();
        append_options.page_size = header.page_size;
        append_options.string_table = header.string_table.clone();
        normalize_encoding_selection(&mut append_options);

        let zstd_compressor =
            new_zstd_compressor(append_options.codec_selection, append_options.zstd_level)?;
        Ok(Self {
            inner: file,
            header,
            key_type,
            options: append_options,
            pending: PendingFrames::new(),
            last_key,
            next_compaction_frame_count: 0,
            compressed_page_frame_hint: 0,
            zstd_compressor,
            packing_stats: PackingStats::default(),
            current_page_attempts: 0,
            append_tail,
        })
    }
}

impl<W: Read + Write + Seek + Resize> Writer<W> {
    pub fn new(mut inner: W, schema: Schema, options: WriterOptions) -> Result<Self> {
        let mut options = options;
        normalize_encoding_selection(&mut options);
        if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&options.page_size) || options.title.is_empty()
        {
            return Err(V2Error::InvalidFileHeader);
        }
        let key_type = KeyType::from_field(schema.key_field())?;
        let header = FileHeader {
            page_size: options.page_size,
            page_count: 0,
            frame_count: 0,
            key_field_index: schema.key_field_index as u16,
            title: options.title.clone(),
            schema,
            string_table: options.string_table.clone(),
        };
        write_file_header(&mut inner, &header)?;
        let zstd_compressor = new_zstd_compressor(options.codec_selection, options.zstd_level)?;
        Ok(Self {
            inner,
            header,
            key_type,
            options,
            pending: PendingFrames::new(),
            last_key: None,
            next_compaction_frame_count: 0,
            compressed_page_frame_hint: 0,
            zstd_compressor,
            packing_stats: PackingStats::default(),
            current_page_attempts: 0,
            append_tail: None,
        })
    }

    pub fn packing_stats(&self) -> PackingStats {
        self.packing_stats
    }

    pub fn header(&self) -> &FileHeader {
        &self.header
    }

    pub fn schema(&self) -> &Schema {
        &self.header.schema
    }

    pub fn frame_count(&self) -> u64 {
        let loaded_tail_frames = self
            .append_tail
            .as_ref()
            .filter(|tail| tail.loaded)
            .map(|tail| tail.frame_count)
            .unwrap_or(0);
        self.header.frame_count + self.pending_frame_count() as u64 - loaded_tail_frames
    }

    pub fn append_frame(&mut self, bytes: &[u8]) -> Result<()> {
        let frame = FrameRef::new(&self.header.schema, bytes)?;
        let key = frame.key(&self.header.schema, self.key_type)?;
        if let Some(last_key) = self.last_key {
            if key < last_key {
                return Err(V2Error::KeyOrderViolation {
                    key,
                    previous: last_key,
                });
            }
        }

        self.pending.append_copy(bytes);
        self.last_key = Some(key);

        self.compact_overflowing_tail()?;
        Ok(())
    }

    pub fn append_raw_frames(&mut self, bytes: &[u8]) -> Result<()> {
        let frame_len = self.header.schema.frame_len as usize;
        if bytes.len() % frame_len != 0 {
            return Err(V2Error::Core(FwobError::InvalidFrameLength {
                expected: frame_len,
                actual: bytes.len(),
            }));
        }
        if bytes.is_empty() {
            return Ok(());
        }

        let mut last_key = self.last_key;
        for frame_bytes in bytes.chunks_exact(frame_len) {
            let frame = FrameRef::new(&self.header.schema, frame_bytes)?;
            let key = frame.key(&self.header.schema, self.key_type)?;
            if let Some(previous) = last_key {
                if key < previous {
                    return Err(V2Error::KeyOrderViolation { key, previous });
                }
            }
            last_key = Some(key);
        }

        self.pending.append_copy(bytes);
        self.last_key = last_key;
        self.compact_overflowing_tail()?;
        Ok(())
    }

    pub fn append_presorted_raw_frames(&mut self, bytes: &[u8]) -> Result<()> {
        let Some(last_key) = self.validate_presorted_raw_frames(bytes)? else {
            return Ok(());
        };
        self.last_key = Some(last_key);
        self.pending.append_copy(bytes);
        self.compact_overflowing_tail()?;
        Ok(())
    }

    /// Appends an already sorted owned frame buffer without copying it into the pending queue.
    pub fn append_presorted_raw_frames_owned(&mut self, bytes: Vec<u8>) -> Result<()> {
        let Some(last_key) = self.validate_presorted_raw_frames(&bytes)? else {
            return Ok(());
        };
        self.last_key = Some(last_key);
        self.pending.append_owned(bytes);
        self.compact_overflowing_tail()?;
        Ok(())
    }

    fn validate_presorted_raw_frames(&self, bytes: &[u8]) -> Result<Option<Key>> {
        let frame_len = self.header.schema.frame_len as usize;
        if bytes.len() % frame_len != 0 {
            return Err(V2Error::Core(FwobError::InvalidFrameLength {
                expected: frame_len,
                actual: bytes.len(),
            }));
        }
        if bytes.is_empty() {
            return Ok(None);
        }

        let first = FrameRef::new(&self.header.schema, &bytes[..frame_len])?;
        let first_key = first.key(&self.header.schema, self.key_type)?;
        if let Some(previous) = self.last_key {
            if first_key < previous {
                return Err(V2Error::KeyOrderViolation {
                    key: first_key,
                    previous,
                });
            }
        }

        let last_offset = bytes.len() - frame_len;
        let last = FrameRef::new(&self.header.schema, &bytes[last_offset..])?;
        Ok(Some(last.key(&self.header.schema, self.key_type)?))
    }

    pub fn append_frames<I, B>(&mut self, frames: I) -> Result<()>
    where
        I: IntoIterator<Item = B>,
        B: AsRef<[u8]>,
    {
        for frame in frames {
            self.append_frame(frame.as_ref())?;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        self.finish_with_stats().map(|_| ())
    }

    pub fn finish_with_stats(mut self) -> Result<PackingStats> {
        self.compact_overflowing_tail()?;
        if self.options.compress_partial_page {
            while !self.pending.is_empty() {
                if !self.flush_one_compressed_page_allow_partial()? {
                    break;
                }
            }
        }
        while !self.pending.is_empty() {
            self.flush_one_raw_page()?;
        }
        self.inner.flush()?;
        Ok(self.packing_stats)
    }

    fn compact_overflowing_tail(&mut self) -> Result<()> {
        while self.should_try_compaction()? {
            if self.options.codec == Codec::None {
                self.flush_one_raw_page()?;
            } else if !self.flush_one_compressed_page()? {
                self.defer_next_compaction_attempt();
                break;
            }
        }
        Ok(())
    }

    fn should_try_compaction(&mut self) -> Result<bool> {
        if self.pending_frame_count() == 0 {
            return Ok(false);
        }
        let pending_frames = self.logical_pending_frame_count();
        let raw_page_frames = self.raw_page_frame_capacity();
        let minimum = if self.options.codec == Codec::None {
            raw_page_frames + 1
        } else {
            raw_page_frames * INITIAL_COMPRESSED_PROBE_RAW_PAGES
        };
        let minimum = self.next_compaction_frame_count.max(minimum);
        if pending_frames < minimum {
            return Ok(false);
        }
        if self.raw_tail_overflows()? {
            Ok(true)
        } else {
            self.defer_next_compaction_attempt();
            Ok(false)
        }
    }

    fn raw_tail_overflows(&mut self) -> Result<bool> {
        if self.pending_frame_count() == 0 {
            return Ok(false);
        }
        if let Some(tail) = self.append_tail.as_ref().filter(|tail| !tail.loaded) {
            let total_frames = tail.frame_count as usize + self.pending_frame_count();
            let encoded_len = self.raw_encoded_len_for_frames(total_frames);
            return Ok(encoded_len > tail.page_count as usize * self.payload_capacity());
        }

        let frame_len = self.header.schema.frame_len as usize;
        let pending_frames = self.pending_frame_count();
        let raw_len = pending_frames * frame_len;
        let (_encoding, encoded) = self.encode_pending_prefix(raw_len, pending_frames)?;
        Ok(encoded.len() > self.payload_capacity())
    }

    fn defer_next_compaction_attempt(&mut self) {
        self.next_compaction_frame_count =
            self.logical_pending_frame_count() + self.raw_page_frame_capacity().max(1);
    }

    fn defer_next_compressed_fit_attempt(&mut self, pending_frames: usize, compressed_len: usize) {
        if compressed_len == 0 {
            self.defer_next_compaction_attempt();
            return;
        }

        let capacity = self.payload_capacity();
        let estimated_full_frames = pending_frames
            .saturating_mul(capacity)
            .div_ceil(compressed_len)
            .saturating_add(1);
        self.next_compaction_frame_count = estimated_full_frames
            .max(pending_frames + self.raw_page_frame_capacity().max(1))
            .max(self.next_compaction_frame_count);
    }

    fn flush_one_compressed_page(&mut self) -> Result<bool> {
        if self.pending.is_empty() {
            return Ok(false);
        }
        self.load_append_tail_for_compression()?;

        let frame_len = self.header.schema.frame_len as usize;
        let pending_frames = self.pending.frame_count(frame_len);
        if pending_frames == 0 {
            return Ok(false);
        }
        let (frame_count, codec, encoding, compressed, encoded_len) =
            self.find_largest_fitting_prefix(pending_frames)?;
        if frame_count == pending_frames {
            self.defer_next_compressed_fit_attempt(pending_frames, compressed.len());
            return Ok(false);
        }
        self.write_compressed_page(frame_count, codec, encoding, compressed, encoded_len)?;
        Ok(true)
    }

    fn flush_one_compressed_page_allow_partial(&mut self) -> Result<bool> {
        if self.pending.is_empty() {
            return Ok(false);
        }
        self.load_append_tail_for_compression()?;
        let frame_len = self.header.schema.frame_len as usize;
        let pending_frames = self.pending.frame_count(frame_len);
        if pending_frames == 0 {
            return Ok(false);
        }
        let raw_len = pending_frames * frame_len;
        let (codec, encoding, compressed, encoded_len) =
            self.compress_pending_prefix(raw_len, pending_frames)?;
        if compressed.len() <= self.payload_capacity() {
            self.write_compressed_page(pending_frames, codec, encoding, compressed, encoded_len)?;
        } else {
            let (frame_count, codec, encoding, compressed, encoded_len) =
                self.find_largest_fitting_prefix(pending_frames)?;
            self.write_compressed_page(frame_count, codec, encoding, compressed, encoded_len)?;
        }
        Ok(true)
    }

    fn write_compressed_page(
        &mut self,
        frame_count: usize,
        codec: Codec,
        encoding: Encoding,
        compressed: Vec<u8>,
        encoded_len: usize,
    ) -> Result<()> {
        self.reclaim_append_tail_for_rewrite()?;
        let frame_len = self.header.schema.frame_len as usize;
        let raw_len = frame_count * frame_len;
        let first_key = self.key_at_offset(0)?;
        let last_key = self.key_at_offset(raw_len - frame_len)?;

        let page_header = PageHeader::new(
            codec,
            encoding,
            first_key,
            last_key,
            frame_count as u32,
            encoded_len as u32,
            compressed.len() as u32,
            self.header.frame_count,
            &compressed,
        );

        let page_offset =
            FILE_HEADER_LEN + self.header.page_count * u64::from(self.header.page_size);
        self.inner.seek(SeekFrom::Start(page_offset))?;
        page_header.write(&mut self.inner)?;
        self.inner.write_all(&compressed)?;
        let written = PAGE_HEADER_LEN + compressed.len();
        self.inner
            .write_all(&vec![0u8; self.header.page_size as usize - written])?;

        self.header.page_count += 1;
        self.header.frame_count += frame_count as u64;
        update_counts(
            &mut self.inner,
            self.header.page_count,
            self.header.frame_count,
        )?;

        self.pending.consume_front(raw_len);
        self.next_compaction_frame_count = 0;
        self.compressed_page_frame_hint = frame_count;
        self.record_compressed_page_attempts();
        if !compressed.is_empty() {
            self.compressed_page_frame_hint = frame_count
                .saturating_mul(self.payload_capacity())
                .div_ceil(compressed.len())
                .max(frame_count);
        }
        Ok(())
    }

    fn flush_one_raw_page(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        // A raw flush rewrites only the trailing run of under-filled pages (for the compressed
        // codec, this is the residual left when compression did not yield a full page; for
        // `Codec::None`, the single trailing under-filled page). Dense leading pages — and any
        // full pages a deferred compression attempt had pulled in — are left on disk untouched.
        self.reclaim_underfilled_tail_for_raw_rewrite()?;
        if self.pending.is_empty() {
            return Ok(());
        }

        let frame_len = self.header.schema.frame_len as usize;
        let pending_frames = self.pending.frame_count(frame_len);
        let frame_count = self.find_largest_raw_page_prefix(pending_frames)?;
        let raw_len = frame_count * frame_len;
        let (encoding, encoded) = self.encode_pending_prefix(raw_len, frame_count)?;
        if encoded.len() > self.payload_capacity() {
            return Err(V2Error::FrameTooLarge);
        }

        let first_key = self.key_at_offset(0)?;
        let last_key = self.key_at_offset(raw_len - frame_len)?;
        let page_header = PageHeader::new(
            Codec::None,
            encoding,
            first_key,
            last_key,
            frame_count as u32,
            encoded.len() as u32,
            encoded.len() as u32,
            self.header.frame_count,
            &encoded,
        );

        let page_offset =
            FILE_HEADER_LEN + self.header.page_count * u64::from(self.header.page_size);
        self.inner.seek(SeekFrom::Start(page_offset))?;
        page_header.write(&mut self.inner)?;
        self.inner.write_all(&encoded)?;
        let written = PAGE_HEADER_LEN + encoded.len();
        self.inner
            .write_all(&vec![0u8; self.header.page_size as usize - written])?;

        self.header.page_count += 1;
        self.header.frame_count += frame_count as u64;
        update_counts(
            &mut self.inner,
            self.header.page_count,
            self.header.frame_count,
        )?;

        self.pending.consume_front(raw_len);
        self.next_compaction_frame_count = 0;
        Ok(())
    }

    /// Drop the whole reclaimed (loaded) append tail from the file and truncate to its start, so
    /// the compressed pages written next replace the entire raw tail (full pages included). Used
    /// only when a compression attempt produced a full compressed page.
    fn reclaim_append_tail_for_rewrite(&mut self) -> Result<()> {
        let Some(tail) = self.append_tail.as_ref() else {
            return Ok(());
        };
        if !tail.loaded {
            return Ok(());
        }
        let tail = self.append_tail.take().expect("tail exists");
        self.header.page_count = tail.start_page;
        self.header.frame_count = self.header.frame_count.saturating_sub(tail.frame_count);
        let new_len = FILE_HEADER_LEN + self.header.page_count * u64::from(self.header.page_size);
        self.inner.resize_len(new_len)?;
        update_counts(
            &mut self.inner,
            self.header.page_count,
            self.header.frame_count,
        )?;
        self.inner.seek(SeekFrom::Start(new_len))?;
        Ok(())
    }

    /// Reclaim only the trailing run of under-filled pages for an in-place raw rewrite, leaving
    /// the dense leading pages of the tail on disk. Handles both an unloaded tail (reads just the
    /// under-filled run) and a tail that a deferred compression attempt already pulled wholesale
    /// into `pending` (drops the dense leading-page frames back off the front).
    fn reclaim_underfilled_tail_for_raw_rewrite(&mut self) -> Result<()> {
        let Some(tail) = self.append_tail.as_ref() else {
            return Ok(());
        };
        let frame_len = self.header.schema.frame_len as usize;
        let underfilled_start = tail.underfilled_start_page;
        let underfilled_frames = tail.underfilled_frame_count;
        let leading_full_frames = tail.frame_count - underfilled_frames;
        let loaded = tail.loaded;

        if loaded {
            // The whole tail sits in `pending` ahead of the new frames; the dense leading pages
            // stay on disk, so drop their frames from the front.
            if leading_full_frames > 0 {
                self.pending
                    .consume_front(leading_full_frames as usize * frame_len);
            }
        } else if underfilled_frames > 0 {
            // Load just the under-filled run, ahead of the pending new frames.
            let new_pending = self.pending.copy_range(0, self.pending.byte_len());
            let mut merged = PendingFrames::new();
            for page_index in underfilled_start..self.header.page_count {
                self.inner
                    .seek(SeekFrom::Start(self.header.page_offset(page_index)))?;
                let page = PageHeader::read(&mut self.inner, page_index)?;
                let mut raw = vec![0u8; page.compressed_len as usize];
                self.inner.read_exact(&mut raw)?;
                page.validate_payload(&raw)?;
                let decoded = decode_page_payload(
                    &self.header.schema,
                    page.encoding,
                    &raw,
                    page.frame_count as usize,
                )?;
                merged.append_owned(decoded);
            }
            merged.append_owned(new_pending);
            self.pending = merged;
        }

        self.append_tail = None;
        // Reclaim only the under-filled run; dense leading pages (`< underfilled_start`) stay put.
        // Truncate the freed pages so a rewrite that shrinks the run leaves no stale pages behind.
        if underfilled_start < self.header.page_count {
            self.header.page_count = underfilled_start;
            self.header.frame_count = self.header.frame_count.saturating_sub(underfilled_frames);
            let new_len =
                FILE_HEADER_LEN + self.header.page_count * u64::from(self.header.page_size);
            self.inner.resize_len(new_len)?;
            update_counts(
                &mut self.inner,
                self.header.page_count,
                self.header.frame_count,
            )?;
            self.inner.seek(SeekFrom::Start(new_len))?;
        }
        self.next_compaction_frame_count = 0;
        Ok(())
    }

    fn load_append_tail_for_compression(&mut self) -> Result<()> {
        let Some(tail) = self.append_tail.as_mut() else {
            return Ok(());
        };
        if tail.loaded {
            return Ok(());
        }

        let new_pending = self.pending.copy_range(0, self.pending.byte_len());
        let mut loaded = PendingFrames::new();
        for page_index in tail.start_page..self.header.page_count {
            self.inner
                .seek(SeekFrom::Start(self.header.page_offset(page_index)))?;
            let page = PageHeader::read(&mut self.inner, page_index)?;
            let mut raw = vec![0u8; page.compressed_len as usize];
            self.inner.read_exact(&mut raw)?;
            page.validate_payload(&raw)?;
            let decoded = decode_page_payload(
                &self.header.schema,
                page.encoding,
                &raw,
                page.frame_count as usize,
            )?;
            loaded.append_owned(decoded);
        }
        loaded.append_owned(new_pending);
        self.pending = loaded;
        tail.loaded = true;
        Ok(())
    }

    fn find_largest_raw_page_prefix(&mut self, pending_frames: usize) -> Result<usize> {
        let frame_len = self.header.schema.frame_len as usize;
        let payload_capacity = self.payload_capacity();
        let per_page_overhead = self.raw_page_encoding_overhead();
        if payload_capacity <= per_page_overhead {
            return Err(V2Error::FrameTooLarge);
        }
        let frame_count = pending_frames.min((payload_capacity - per_page_overhead) / frame_len);
        if frame_count == 0 {
            return Err(V2Error::FrameTooLarge);
        }
        Ok(frame_count)
    }

    fn raw_page_encoding_overhead(&self) -> usize {
        match self.options.encoding_selection {
            EncodingSelection::Fixed(Encoding::ColumnarDeltaV1) => self.header.schema.fields.len(),
            EncodingSelection::Fixed(Encoding::RowRawV1 | Encoding::ColumnarBasicV1)
            | EncodingSelection::Smallest => 0,
        }
    }

    fn raw_encoded_len_for_frames(&self, frame_count: usize) -> usize {
        frame_count * self.header.schema.frame_len as usize + self.raw_page_encoding_overhead()
    }

    fn find_largest_fitting_prefix(&mut self, candidate_frames: usize) -> Result<FittingCandidate> {
        match self.options.page_packing {
            PagePacking::EstimateShrink => self.find_estimated_fitting_prefix(candidate_frames),
            PagePacking::TightFit => self.find_gradient_fitting_prefix(candidate_frames),
        }
    }

    fn find_estimated_fitting_prefix(
        &mut self,
        candidate_frames: usize,
    ) -> Result<FittingCandidate> {
        let frame_len = self.header.schema.frame_len as usize;
        if self.compressed_page_frame_hint == 0 {
            return self.find_gradient_fitting_prefix(candidate_frames);
        }

        let probe = self.compressed_page_frame_hint.min(candidate_frames).max(1);
        let raw_len = probe * frame_len;
        let (codec, encoding, compressed, encoded_len) =
            self.compress_pending_prefix(raw_len, probe)?;
        if compressed.len() <= self.payload_capacity() {
            return Ok((probe, codec, encoding, compressed, encoded_len));
        }

        let mut probe = probe;
        let mut compressed_len = compressed.len();
        loop {
            let estimated = probe
                .saturating_mul(self.payload_capacity())
                .checked_div(compressed_len.max(1))
                .unwrap_or(0)
                .min(probe.saturating_sub(1))
                .max(1);
            if estimated == probe {
                return Err(V2Error::FrameTooLarge);
            }

            let raw_len = estimated * frame_len;
            let (codec, encoding, compressed, encoded_len) =
                self.compress_pending_prefix(raw_len, estimated)?;
            if compressed.len() <= self.payload_capacity() {
                return Ok((estimated, codec, encoding, compressed, encoded_len));
            }

            probe = estimated;
            compressed_len = compressed.len();
        }
    }

    fn find_gradient_fitting_prefix(
        &mut self,
        candidate_frames: usize,
    ) -> Result<FittingCandidate> {
        let frame_len = self.header.schema.frame_len as usize;
        let raw_page_frames = self.raw_page_frame_capacity();
        let initial_probe = if self.compressed_page_frame_hint > 0 {
            self.compressed_page_frame_hint
        } else {
            raw_page_frames.saturating_mul(INITIAL_COMPRESSED_PROBE_RAW_PAGES)
        };
        let mut probe = initial_probe.min(candidate_frames).max(1);
        let mut best: Option<FittingCandidate> = None;
        let mut overflow: Option<(usize, usize)> = None;
        let mut initial_window_attempts = 0u64;
        let mut window_bounds = [None; RECORDED_WINDOW_SPANS];
        let mut recorded_window_spans = 0usize;

        for _ in 0..32 {
            let recorded_window_spans_before_attempt = recorded_window_spans;
            initial_window_attempts += 1;
            let raw_len = probe * frame_len;
            let (codec, encoding, compressed, encoded_len) =
                self.compress_pending_prefix(raw_len, probe)?;
            if compressed.len() <= self.payload_capacity() {
                best = Some((probe, codec, encoding, compressed, encoded_len));
                if recorded_window_spans == 0 {
                    if let Some((overflow_frame, _)) = overflow {
                        self.record_initial_window(overflow_frame - probe, initial_window_attempts);
                        window_bounds[0] = Some((probe, overflow_frame));
                        recorded_window_spans = 1;
                    }
                }
                if probe == candidate_frames {
                    self.record_window_final_positions(&window_bounds, probe);
                    return Ok(best.expect("best is set"));
                }

                let mut ratio_next = probe
                    .saturating_mul(self.payload_capacity())
                    .checked_div(best.as_ref().expect("best is set").3.len().max(1))
                    .unwrap_or(probe)
                    .min(candidate_frames);
                if let Some((overflow_frame, _)) = overflow {
                    ratio_next = ratio_next.min(overflow_frame.saturating_sub(1));
                }
                let next = ratio_next;
                if next == probe {
                    self.record_window_final_positions(&window_bounds, probe);
                    return Ok(best.expect("best is set"));
                }
                probe = next;
            } else {
                overflow = Some((probe, compressed.len()));
                if let Some((fit, _, _, _, _)) = best.as_ref() {
                    if recorded_window_spans == 0 {
                        self.record_initial_window(probe - *fit, initial_window_attempts);
                        window_bounds[0] = Some((*fit, probe));
                        recorded_window_spans = 1;
                    }
                    if probe <= fit + 1 {
                        break;
                    }
                    let ratio_next = interpolated_probe(
                        *fit,
                        best.as_ref().expect("best is set").3.len(),
                        probe,
                        compressed.len(),
                        self.payload_capacity(),
                        *fit + 1,
                        probe - 1,
                    );
                    if ratio_next == probe {
                        break;
                    }
                    probe = ratio_next;
                } else {
                    let ratio_next = probe
                        .saturating_mul(self.payload_capacity())
                        .checked_div(compressed.len().max(1))
                        .unwrap_or(0)
                        .min(probe.saturating_sub(1))
                        .max(1);
                    let next = ratio_next.max(1);
                    if next == probe {
                        break;
                    }
                    probe = next;
                }
            }
            if recorded_window_spans_before_attempt > 0 {
                if let Some(bounds) =
                    self.record_next_window_span(&best, overflow, &mut recorded_window_spans)
                {
                    window_bounds[recorded_window_spans - 1] = Some(bounds);
                }
            }
        }

        if best.is_none() {
            return Err(V2Error::FrameTooLarge);
        }

        if let Some((overflow, overflow_len)) = overflow {
            let (mut lo_frame, mut lo_len) = {
                let (frames, _, _, compressed, _) = best.as_ref().expect("best is set");
                (*frames, compressed.len())
            };
            let mut lo = lo_frame + 1;
            let mut hi = overflow - 1;
            let mut hi_frame = overflow;
            let mut hi_len = overflow_len;
            while lo <= hi {
                let ratio_probe = interpolated_probe(
                    lo_frame,
                    lo_len,
                    hi_frame,
                    hi_len,
                    self.payload_capacity(),
                    lo,
                    hi,
                );
                let raw_len = ratio_probe * frame_len;
                let (codec, encoding, compressed, encoded_len) =
                    self.compress_pending_prefix(raw_len, ratio_probe)?;
                if compressed.len() <= self.payload_capacity() {
                    lo_frame = ratio_probe;
                    lo_len = compressed.len();
                    best = Some((ratio_probe, codec, encoding, compressed, encoded_len));
                    lo = ratio_probe + 1;
                } else if ratio_probe == 1 {
                    if recorded_window_spans < RECORDED_WINDOW_SPANS {
                        self.record_window_span(recorded_window_spans, hi_frame - lo_frame);
                        window_bounds[recorded_window_spans] = Some((lo_frame, hi_frame));
                    }
                    break;
                } else {
                    hi = ratio_probe - 1;
                    hi_frame = ratio_probe;
                    hi_len = compressed.len();
                }
                if recorded_window_spans < RECORDED_WINDOW_SPANS {
                    self.record_window_span(recorded_window_spans, hi_frame - lo_frame);
                    window_bounds[recorded_window_spans] = Some((lo_frame, hi_frame));
                    recorded_window_spans += 1;
                }
            }
        }

        if let Some((final_fit, _, _, _, _)) = best.as_ref() {
            self.record_window_final_positions(&window_bounds, *final_fit);
        }

        best.ok_or(V2Error::FrameTooLarge)
    }

    fn compress_pending_prefix(
        &mut self,
        raw_len: usize,
        frame_count: usize,
    ) -> Result<CompressedCandidate> {
        self.current_page_attempts += 1;
        let schema = self.header.schema.clone();
        match self.options.encoding_selection {
            EncodingSelection::Fixed(encoding) => {
                let raw = self.pending.prefix_contiguous(raw_len);
                let encoded = encode_page_payload(&schema, encoding, raw, frame_count)?;
                self.compress_encoded(encoding, encoded)
            }
            EncodingSelection::Smallest => {
                let mut best: Option<CompressedCandidate> = None;
                for encoding in [Encoding::ColumnarBasicV1, Encoding::ColumnarDeltaV1] {
                    let raw = self.pending.prefix_contiguous(raw_len);
                    let encoded = encode_page_payload(&schema, encoding, raw, frame_count)?;
                    let candidate = self.compress_encoded(encoding, encoded)?;
                    if best.as_ref().is_none_or(|(_, _, best_compressed, _)| {
                        candidate.2.len() < best_compressed.len()
                    }) {
                        best = Some(candidate);
                    }
                }
                Ok(best.expect("smallest encoding has candidates"))
            }
        }
    }

    fn compress_encoded(
        &mut self,
        encoding: Encoding,
        encoded: Vec<u8>,
    ) -> Result<CompressedCandidate> {
        let encoded_len = encoded.len();
        match self.options.codec_selection {
            CodecSelection::Fixed(codec) => {
                let compressed = match codec {
                    Codec::Zstd => self
                        .zstd_compressor
                        .as_mut()
                        .expect("zstd compressor is initialized")
                        .compress(&encoded)?,
                    _ => codec.compress_with_zstd_level(&encoded, self.options.zstd_level)?,
                };
                Ok((codec, encoding, compressed, encoded_len))
            }
            CodecSelection::Smallest => {
                let mut best_codec = Codec::None;
                let mut best =
                    Codec::None.compress_with_zstd_level(&encoded, self.options.zstd_level)?;
                for codec in [Codec::Lz4, Codec::Zstd] {
                    let compressed = match codec {
                        Codec::Zstd => self
                            .zstd_compressor
                            .as_mut()
                            .expect("zstd compressor is initialized")
                            .compress(&encoded)?,
                        _ => codec.compress_with_zstd_level(&encoded, self.options.zstd_level)?,
                    };
                    if compressed.len() < best.len() {
                        best_codec = codec;
                        best = compressed;
                    }
                }
                Ok((best_codec, encoding, best, encoded_len))
            }
        }
    }

    fn encode_pending_prefix(
        &mut self,
        raw_len: usize,
        frame_count: usize,
    ) -> Result<(Encoding, Vec<u8>)> {
        let schema = self.header.schema.clone();
        match self.options.encoding_selection {
            EncodingSelection::Fixed(encoding) => Ok((
                encoding,
                encode_page_payload(
                    &schema,
                    encoding,
                    self.pending.prefix_contiguous(raw_len),
                    frame_count,
                )?,
            )),
            EncodingSelection::Smallest => {
                let mut best: Option<(Encoding, Vec<u8>)> = None;
                for encoding in [Encoding::ColumnarBasicV1, Encoding::ColumnarDeltaV1] {
                    let raw = self.pending.prefix_contiguous(raw_len);
                    let encoded = encode_page_payload(&schema, encoding, raw, frame_count)?;
                    if best
                        .as_ref()
                        .is_none_or(|(_, best_encoded)| encoded.len() < best_encoded.len())
                    {
                        best = Some((encoding, encoded));
                    }
                }
                Ok(best.expect("smallest encoding has candidates"))
            }
        }
    }

    fn key_at_offset(&self, offset: usize) -> Result<Key> {
        let frame_len = self.header.schema.frame_len as usize;
        let frame_bytes = self.pending.copy_range(offset, frame_len);
        let frame = FrameRef::new(&self.header.schema, &frame_bytes)?;
        Ok(frame.key(&self.header.schema, self.key_type)?)
    }

    fn payload_capacity(&self) -> usize {
        self.header.page_size as usize - PAGE_HEADER_LEN
    }

    fn pending_frame_count(&self) -> usize {
        self.pending
            .frame_count(self.header.schema.frame_len as usize)
    }

    fn logical_pending_frame_count(&self) -> usize {
        self.pending_frame_count()
            + self
                .append_tail
                .as_ref()
                .filter(|tail| !tail.loaded)
                .map(|tail| tail.frame_count as usize)
                .unwrap_or(0)
    }

    fn raw_page_frame_capacity(&self) -> usize {
        let frame_len = self.header.schema.frame_len as usize;
        (self.payload_capacity() / frame_len).max(1)
    }

    fn record_compressed_page_attempts(&mut self) {
        let attempts = self.current_page_attempts;
        self.current_page_attempts = 0;
        if attempts == 0 {
            return;
        }
        if self.packing_stats.first_page_attempts == 0 {
            self.packing_stats.first_page_attempts = attempts;
        } else {
            self.packing_stats.subsequent_page_attempts += attempts;
            self.packing_stats.subsequent_pages += 1;
            if self.packing_stats.subsequent_min_attempts == 0 {
                self.packing_stats.subsequent_min_attempts = attempts;
            } else {
                self.packing_stats.subsequent_min_attempts =
                    self.packing_stats.subsequent_min_attempts.min(attempts);
            }
            self.packing_stats.subsequent_max_attempts =
                self.packing_stats.subsequent_max_attempts.max(attempts);
        }
    }

    fn record_initial_window(&mut self, frame_count: usize, attempts: u64) {
        self.packing_stats.initial_windows += 1;
        self.packing_stats.initial_window_attempts += attempts;
        self.record_window_span(0, frame_count);
    }

    fn record_window_span(&mut self, index: usize, frame_count: usize) {
        if index >= RECORDED_WINDOW_SPANS {
            return;
        }
        self.packing_stats.window_span_counts[index] += 1;
        self.packing_stats.window_frame_spans[index] += frame_count as u64;
    }

    fn record_next_window_span(
        &mut self,
        best: &Option<FittingCandidate>,
        overflow: Option<(usize, usize)>,
        recorded_window_spans: &mut usize,
    ) -> Option<(usize, usize)> {
        if *recorded_window_spans >= RECORDED_WINDOW_SPANS {
            return None;
        }
        if let (Some((fit, _, _, _, _)), Some((overflow, _))) = (best.as_ref(), overflow) {
            self.record_window_span(*recorded_window_spans, overflow - *fit);
            *recorded_window_spans += 1;
            Some((*fit, overflow))
        } else {
            None
        }
    }

    fn record_window_final_position(
        &mut self,
        index: usize,
        fit: usize,
        overflow: usize,
        final_fit: usize,
    ) {
        if index >= RECORDED_WINDOW_SPANS || overflow <= fit {
            return;
        }
        let position = (final_fit - fit) as f64 / (overflow - fit) as f64;
        self.packing_stats.window_final_position_sums[index] += position;
        self.packing_stats.window_final_position_counts[index] += 1;
    }

    fn record_window_final_positions(
        &mut self,
        window_bounds: &[Option<(usize, usize)>; RECORDED_WINDOW_SPANS],
        final_fit: usize,
    ) {
        for (index, bounds) in window_bounds.iter().enumerate() {
            if let Some((fit, overflow)) = bounds {
                self.record_window_final_position(index, *fit, *overflow, final_fit);
            }
        }
    }
}

fn interpolated_probe(
    fit_frame: usize,
    _fit_len: usize,
    overflow_frame: usize,
    overflow_len: usize,
    target_len: usize,
    min_frame: usize,
    max_frame: usize,
) -> usize {
    if min_frame >= max_frame {
        return min_frame;
    }
    if overflow_frame <= fit_frame || overflow_len == 0 {
        return max_frame;
    }

    let estimated = overflow_frame
        .saturating_mul(target_len)
        .checked_div(overflow_len)
        .unwrap_or(min_frame)
        .max(fit_frame + 1)
        .min(overflow_frame - 1);
    let window = max_frame - min_frame + 1;
    if window <= 2 {
        return estimated.clamp(min_frame, max_frame);
    }
    if INTERPOLATED_PROBE_WINDOW_MARGIN_DIVISOR == 0 {
        return estimated.clamp(min_frame, max_frame);
    }
    let margin = (window / INTERPOLATED_PROBE_WINDOW_MARGIN_DIVISOR).max(1);
    let clamped_min = min_frame.saturating_add(margin).min(max_frame);
    let clamped_max = max_frame.saturating_sub(margin).max(min_frame);
    if clamped_min > clamped_max {
        min_frame + ((max_frame - min_frame) >> 1)
    } else {
        estimated.clamp(clamped_min, clamped_max)
    }
}

fn normalize_encoding_selection(options: &mut WriterOptions) {
    if matches!(options.encoding_selection, EncodingSelection::Fixed(_)) {
        options.encoding_selection = EncodingSelection::Fixed(options.encoding);
    }
}

fn new_zstd_compressor(
    codec_selection: CodecSelection,
    zstd_level: i32,
) -> Result<Option<zstd::bulk::Compressor<'static>>> {
    let needs_zstd = matches!(codec_selection, CodecSelection::Fixed(Codec::Zstd))
        || matches!(codec_selection, CodecSelection::Smallest);
    if needs_zstd {
        Ok(Some(zstd::bulk::Compressor::new(zstd_level)?))
    } else {
        Ok(None)
    }
}
