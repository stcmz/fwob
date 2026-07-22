use super::*;

pub(super) enum StorageSummary {
    V1 {
        frame_count: u64,
        physical_bytes: u64,
        raw_bytes: u64,
    },
    V2(V2Metadata),
}

impl StorageSummary {
    pub(super) fn empty(format: fwob_core::FormatVersion, page_size: u32) -> Self {
        match format {
            fwob_core::FormatVersion::V1 => Self::V1 {
                frame_count: 0,
                physical_bytes: 0,
                raw_bytes: 0,
            },
            fwob_core::FormatVersion::V2 => Self::V2(V2Metadata {
                payload_capacity_per_page: u64::from(page_size)
                    .saturating_sub(fwob_v2::PAGE_HEADER_LEN as u64),
                ..Default::default()
            }),
        }
    }

    pub(super) fn collect(paths: &[PathBuf], key_field_index: usize) -> Result<Self> {
        let Some(first) = paths.first() else {
            bail!("cannot infer storage format from an empty output list");
        };
        match detect_format(first)? {
            Format::V1 => {
                let mut physical_bytes = 0u64;
                let mut raw_bytes = 0u64;
                let mut frame_count = 0u64;
                for path in paths {
                    if !matches!(detect_format(path)?, Format::V1) {
                        bail!("operation outputs use mixed format versions");
                    }
                    let reader = fwob_v1::Reader::open(path, key_field_index)?;
                    physical_bytes = physical_bytes.saturating_add(std::fs::metadata(path)?.len());
                    frame_count = frame_count.saturating_add(reader.frame_count());
                    raw_bytes = raw_bytes.saturating_add(
                        reader
                            .frame_count()
                            .saturating_mul(u64::from(reader.header().frame_length)),
                    );
                }
                Ok(Self::V1 {
                    frame_count,
                    physical_bytes,
                    raw_bytes,
                })
            }
            Format::V2 => {
                let mut aggregate = None;
                for path in paths {
                    if !matches!(detect_format(path)?, Format::V2) {
                        bail!("operation outputs use mixed format versions");
                    }
                    let mut reader = fwob_v2::Reader::open(path)?;
                    let metadata = collect_v2_metadata(path, &mut reader)?;
                    if let Some(current) = &mut aggregate {
                        merge_v2_metadata(current, metadata);
                    } else {
                        aggregate = Some(metadata);
                    }
                }
                Ok(Self::V2(aggregate.expect("nonempty output list")))
            }
        }
    }

    pub(super) fn format_name(&self) -> &'static str {
        match self {
            Self::V1 { .. } => "fwob-v1",
            Self::V2(_) => "fwob-v2",
        }
    }

    pub(super) fn v2_metadata(&self) -> Option<&V2Metadata> {
        match self {
            Self::V1 { .. } => None,
            Self::V2(metadata) => Some(metadata),
        }
    }

    pub(super) fn frame_count(&self) -> u64 {
        match self {
            Self::V1 { frame_count, .. } => *frame_count,
            Self::V2(metadata) => metadata.frame_count,
        }
    }

    pub(super) fn page_count(&self) -> Option<u64> {
        self.v2_metadata().map(|metadata| metadata.page_count)
    }
}

fn merge_v2_metadata(target: &mut V2Metadata, source: V2Metadata) {
    target.frame_count = target.frame_count.saturating_add(source.frame_count);
    target.page_count = target.page_count.saturating_add(source.page_count);
    target.physical_bytes = target.physical_bytes.saturating_add(source.physical_bytes);
    target.expected_physical_bytes = target
        .expected_physical_bytes
        .saturating_add(source.expected_physical_bytes);
    target.payload_capacity_total = target
        .payload_capacity_total
        .saturating_add(source.payload_capacity_total);
    target.compressed_total = target
        .compressed_total
        .saturating_add(source.compressed_total);
    target.uncompressed_total = target
        .uncompressed_total
        .saturating_add(source.uncompressed_total);
    target.padding_bytes = target.padding_bytes.saturating_add(source.padding_bytes);
    target.min_frames = target.min_frames.min(source.min_frames);
    target.max_frames = target.max_frames.max(source.max_frames);
    target.first_key = target.first_key.or(source.first_key);
    target.last_key = source.last_key.or(target.last_key);
    target.codec_none_pages = target
        .codec_none_pages
        .saturating_add(source.codec_none_pages);
    target.codec_zstd_pages = target
        .codec_zstd_pages
        .saturating_add(source.codec_zstd_pages);
    target.codec_lz4_pages = target
        .codec_lz4_pages
        .saturating_add(source.codec_lz4_pages);
    target.compressed_page_frames = target
        .compressed_page_frames
        .saturating_add(source.compressed_page_frames);
    target.encoding_row_raw_v1_pages = target
        .encoding_row_raw_v1_pages
        .saturating_add(source.encoding_row_raw_v1_pages);
    target.encoding_columnar_basic_v1_pages = target
        .encoding_columnar_basic_v1_pages
        .saturating_add(source.encoding_columnar_basic_v1_pages);
    target.encoding_columnar_delta_v1_pages = target
        .encoding_columnar_delta_v1_pages
        .saturating_add(source.encoding_columnar_delta_v1_pages);
}

pub(super) struct OperationResult<'a> {
    pub section: &'a str,
    pub storage: &'a StorageSummary,
    pub input: Option<&'a Path>,
    pub output: &'a Path,
    pub input_count: usize,
    pub verified: bool,
    pub elapsed_seconds: f64,
}

pub(super) fn print_operation_result(result: OperationResult<'_>) -> std::io::Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    w.section(result.section)?;
    w.kv_str("target_format", result.storage.format_name())?;
    if let Some(input) = result.input {
        w.kv_str("input", &input.display().to_string())?;
    }
    w.kv_str("output", &result.output.display().to_string())?;
    w.kv_num("input_count", result.input_count)?;
    w.kv_num("frames", result.storage.frame_count())?;
    if let Some(page_count) = result.storage.page_count() {
        w.kv_num("pages", page_count)?;
    }
    w.kv_bool("verified", result.verified)?;
    w.kv_str(
        "verification",
        if result.verified {
            "completed"
        } else {
            "skipped (run `fwob verify` or pass verify)"
        },
    )?;
    w.kv_float("elapsed_seconds", result.elapsed_seconds, 3)?;
    Ok(())
}

pub(super) struct CommonSummary<'a> {
    pub storage: &'a StorageSummary,
    pub key_field_index: usize,
    pub page_size: Option<u32>,
    pub write: Option<V2WriteOptions>,
    pub packing: Option<fwob_v2::PackingStats>,
    pub parallelism: Option<usize>,
    pub verified: bool,
}

pub(super) fn print_common_sections(summary: CommonSummary<'_>) -> std::io::Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    println!();
    w.section("parameters")?;
    w.kv_str("target_format", summary.storage.format_name())?;
    w.kv_num("key_field_index", summary.key_field_index)?;
    if let Some(page_size) = summary.page_size {
        w.kv_num("page_size", page_size)?;
    }
    if let Some(parallelism) = summary.parallelism {
        w.kv_num("parallelism", parallelism)?;
    }
    if let Some(write) = summary.write {
        w.kv_str("codec", write.codec.as_str())?;
        w.kv_str("encoding", write.encoding.as_str())?;
        w.kv_num("zstd_level", write.zstd_level)?;
        w.kv_bool("compress_partial_page", write.compress_partial_page)?;
        w.kv_str("page_packing", write.page_packing.as_str())?;
    }
    w.kv_bool("verify", summary.verified)?;

    println!();
    if let Some(packing) = summary.packing {
        print_packing_stats_toml(packing)?;
    } else {
        w.section("packing")?;
        w.kv_bool("available", false)?;
    }

    println!();
    w.section("compression")?;
    match summary.storage {
        StorageSummary::V1 {
            physical_bytes,
            raw_bytes,
            ..
        } => {
            w.kv_num("compressed_payload_bytes", *raw_bytes)?;
            w.kv_num("uncompressed_payload_bytes", *raw_bytes)?;
            if *raw_bytes > 0 {
                w.kv_float("payload_ratio", 1.0, 4)?;
                w.kv_float(
                    "physical_ratio",
                    *physical_bytes as f64 / *raw_bytes as f64,
                    4,
                )?;
            }
        }
        StorageSummary::V2(metadata) => print_compression_stats_values(metadata)?,
    }

    println!();
    w.section("page_stats")?;
    if let Some(metadata) = summary.storage.v2_metadata() {
        print_page_codec_encoding_stats_toml(metadata)?;
    } else {
        w.kv_bool("available", false)?;
    }
    Ok(())
}

pub(super) fn print_packing_stats_toml(stats: fwob_v2::PackingStats) -> std::io::Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    w.section("packing")?;
    w.kv_num("first_page_compression_attempts", stats.first_page_attempts)?;
    w.kv_float(
        "subsequent_page_average_compression_attempts",
        stats.subsequent_average_attempts(),
        2,
    )?;
    w.kv_str(
        "subsequent_page_compression_attempts_range",
        &format!(
            "{}..{}",
            stats.subsequent_min_attempts, stats.subsequent_max_attempts
        ),
    )?;
    w.kv_num(
        "subsequent_compressed_pages_measured",
        stats.subsequent_pages,
    )?;
    let spans = (0..10)
        .map(|index| stats.average_window_frame_span(index))
        .collect::<Vec<_>>();
    w.kv_float_array("average_initial_window_frame_span", &spans, 2)?;
    w.kv_float(
        "average_initial_window_attempts",
        stats.average_initial_window_attempts(),
        2,
    )?;
    let positions = (0..10)
        .map(|index| stats.average_window_final_position(index))
        .collect::<Vec<_>>();
    w.kv_float_array("average_initial_window_final_position", &positions, 4)?;
    w.kv_num("initial_windows_measured", stats.initial_windows)?;
    Ok(())
}

fn print_compression_stats_values(metadata: &V2Metadata) -> std::io::Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    w.kv_num("compressed_payload_bytes", metadata.compressed_total)?;
    w.kv_num("uncompressed_payload_bytes", metadata.uncompressed_total)?;
    let compressed_pages = metadata.codec_zstd_pages + metadata.codec_lz4_pages;
    if compressed_pages > 0 {
        w.kv_float(
            "average_frames_per_compressed_page",
            metadata.compressed_page_frames as f64 / compressed_pages as f64,
            2,
        )?;
    }
    if metadata.uncompressed_total > 0 {
        w.kv_float(
            "payload_ratio",
            metadata.compressed_total as f64 / metadata.uncompressed_total as f64,
            4,
        )?;
        w.kv_float(
            "physical_ratio",
            metadata.physical_bytes as f64 / metadata.uncompressed_total as f64,
            4,
        )?;
    }
    Ok(())
}

pub(super) fn print_page_codec_encoding_stats_toml(metadata: &V2Metadata) -> std::io::Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    w.kv_num("codec_none_pages", metadata.codec_none_pages)?;
    w.kv_num("codec_zstd_pages", metadata.codec_zstd_pages)?;
    w.kv_num("codec_lz4_pages", metadata.codec_lz4_pages)?;
    w.kv_num(
        "encoding_row_raw_v1_pages",
        metadata.encoding_row_raw_v1_pages,
    )?;
    w.kv_num(
        "encoding_columnar_basic_v1_pages",
        metadata.encoding_columnar_basic_v1_pages,
    )?;
    w.kv_num(
        "encoding_columnar_delta_v1_pages",
        metadata.encoding_columnar_delta_v1_pages,
    )?;
    Ok(())
}
