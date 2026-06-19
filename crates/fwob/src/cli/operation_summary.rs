use super::*;

pub(super) enum StorageSummary {
    V1 { physical_bytes: u64, raw_bytes: u64 },
    V2(V2Metadata),
}

impl StorageSummary {
    pub(super) fn empty(format: fwob_core::FormatVersion, page_size: u32) -> Self {
        match format {
            fwob_core::FormatVersion::V1 => Self::V1 {
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
                for path in paths {
                    if !matches!(detect_format(path)?, Format::V1) {
                        bail!("operation outputs use mixed format versions");
                    }
                    let reader = fwob_v1::Reader::open(path, key_field_index)?;
                    physical_bytes = physical_bytes.saturating_add(std::fs::metadata(path)?.len());
                    raw_bytes = raw_bytes.saturating_add(
                        reader
                            .frame_count()
                            .saturating_mul(u64::from(reader.header().frame_length)),
                    );
                }
                Ok(Self::V1 {
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
}

fn merge_v2_metadata(target: &mut V2Metadata, source: V2Metadata) {
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

pub(super) struct CommonSummary<'a> {
    pub storage: &'a StorageSummary,
    pub key_field_index: usize,
    pub page_size: Option<u32>,
    pub write: Option<V2WriteOptions>,
    pub packing: Option<fwob_v2::PackingStats>,
    pub parallelism: Option<usize>,
    pub verified: bool,
}

pub(super) fn print_common_sections(summary: CommonSummary<'_>) {
    println!();
    toml_section("parameters");
    toml_kv_str("target_format", summary.storage.format_name());
    toml_kv_num("key_field_index", summary.key_field_index);
    if let Some(page_size) = summary.page_size {
        toml_kv_num("page_size", page_size);
    }
    if let Some(parallelism) = summary.parallelism {
        toml_kv_num("parallelism", parallelism);
    }
    if let Some(write) = summary.write {
        toml_kv_str("codec", write.codec.as_str());
        toml_kv_str("encoding", write.encoding.as_str());
        toml_kv_num("zstd_level", write.zstd_level);
        toml_kv_bool("compress_partial_page", write.compress_partial_page);
        toml_kv_str("page_packing", write.page_packing.as_str());
    }
    toml_kv_bool("verify", summary.verified);

    println!();
    if let Some(packing) = summary.packing {
        print_packing_stats_toml(packing);
    } else {
        toml_section("packing");
        toml_kv_bool("available", false);
    }

    println!();
    toml_section("compression");
    match summary.storage {
        StorageSummary::V1 {
            physical_bytes,
            raw_bytes,
        } => {
            toml_kv_num("compressed_payload_bytes", raw_bytes);
            toml_kv_num("uncompressed_payload_bytes", raw_bytes);
            if *raw_bytes > 0 {
                toml_kv_num("payload_ratio", "1.0000");
                toml_kv_num(
                    "physical_ratio",
                    format!("{:.4}", *physical_bytes as f64 / *raw_bytes as f64),
                );
            }
        }
        StorageSummary::V2(metadata) => print_compression_stats_values(metadata),
    }

    println!();
    toml_section("page_stats");
    if let Some(metadata) = summary.storage.v2_metadata() {
        print_page_codec_encoding_stats_toml(metadata);
    } else {
        toml_kv_bool("available", false);
    }
}

pub(super) fn print_packing_stats_toml(stats: fwob_v2::PackingStats) {
    toml_section("packing");
    toml_kv_num("first_page_compression_attempts", stats.first_page_attempts);
    toml_kv_num(
        "subsequent_page_average_compression_attempts",
        format!("{:.2}", stats.subsequent_average_attempts()),
    );
    toml_kv_str(
        "subsequent_page_compression_attempts_range",
        &format!(
            "{}..{}",
            stats.subsequent_min_attempts, stats.subsequent_max_attempts
        ),
    );
    toml_kv_num(
        "subsequent_compressed_pages_measured",
        stats.subsequent_pages,
    );
    let spans = (0..10)
        .map(|index| stats.average_window_frame_span(index))
        .collect::<Vec<_>>();
    toml_kv_float_array("average_initial_window_frame_span", &spans, 2);
    toml_kv_num(
        "average_initial_window_attempts",
        format!("{:.2}", stats.average_initial_window_attempts()),
    );
    let positions = (0..10)
        .map(|index| stats.average_window_final_position(index))
        .collect::<Vec<_>>();
    toml_kv_float_array("average_initial_window_final_position", &positions, 4);
    toml_kv_num("initial_windows_measured", stats.initial_windows);
}

fn print_compression_stats_values(metadata: &V2Metadata) {
    toml_kv_num("compressed_payload_bytes", metadata.compressed_total);
    toml_kv_num("uncompressed_payload_bytes", metadata.uncompressed_total);
    let compressed_pages = metadata.codec_zstd_pages + metadata.codec_lz4_pages;
    if compressed_pages > 0 {
        toml_kv_num(
            "average_frames_per_compressed_page",
            format!(
                "{:.2}",
                metadata.compressed_page_frames as f64 / compressed_pages as f64
            ),
        );
    }
    if metadata.uncompressed_total > 0 {
        toml_kv_num(
            "payload_ratio",
            format!(
                "{:.4}",
                metadata.compressed_total as f64 / metadata.uncompressed_total as f64
            ),
        );
        toml_kv_num(
            "physical_ratio",
            format!(
                "{:.4}",
                metadata.physical_bytes as f64 / metadata.uncompressed_total as f64
            ),
        );
    }
}

pub(super) fn print_page_codec_encoding_stats_toml(metadata: &V2Metadata) {
    toml_kv_num("codec_none_pages", metadata.codec_none_pages);
    toml_kv_num("codec_zstd_pages", metadata.codec_zstd_pages);
    toml_kv_num("codec_lz4_pages", metadata.codec_lz4_pages);
    toml_kv_num(
        "encoding_row_raw_v1_pages",
        metadata.encoding_row_raw_v1_pages,
    );
    toml_kv_num(
        "encoding_columnar_basic_v1_pages",
        metadata.encoding_columnar_basic_v1_pages,
    );
    toml_kv_num(
        "encoding_columnar_delta_v1_pages",
        metadata.encoding_columnar_delta_v1_pages,
    );
}
