use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

use super::*;

static CONVERSION_OUTPUT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
struct ConversionJob {
    input: PathBuf,
    output: PathBuf,
}

pub(super) fn convert(args: ConvertArgs) -> Result<()> {
    let (target_format, input, output, page_size, write) =
        parse_convert_target(&args.target, args.write)?;
    validate_zstd_level(write.zstd_level)?;
    let jobs = plan_conversion_jobs(&input, &output)?;
    let parallelism = args
        .parallelism
        .map(std::num::NonZeroUsize::get)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from))
        .min(jobs.len().max(1));
    run_conversion_jobs(
        &jobs,
        parallelism,
        target_format,
        args.key_field_index,
        page_size,
        write,
    )
}

fn plan_conversion_jobs(input: &Path, output: &Path) -> Result<Vec<ConversionJob>> {
    let input_metadata = std::fs::metadata(input)
        .with_context(|| format!("failed to inspect conversion input {}", input.display()))?;
    if input_metadata.is_file() {
        let output_is_directory =
            output.is_dir() || (!output.exists() && output.extension().is_none());
        let destination = if output_is_directory {
            std::fs::create_dir_all(output).with_context(|| {
                format!("failed to create output directory {}", output.display())
            })?;
            output.join(
                input
                    .file_name()
                    .context("conversion input file has no filename")?,
            )
        } else {
            output.to_owned()
        };
        ensure_distinct_paths(input, &destination)?;
        return Ok(vec![ConversionJob {
            input: input.to_owned(),
            output: destination,
        }]);
    }
    if !input_metadata.is_dir() {
        bail!("conversion input must be a regular file or directory");
    }
    if output.exists() && !output.is_dir() {
        bail!("a directory input requires a directory output");
    }

    let mut inputs = Vec::new();
    for entry in std::fs::read_dir(input)
        .with_context(|| format!("failed to read input directory {}", input.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() && has_fwob_extension(&path) {
            inputs.push(path);
        }
    }
    inputs.sort_by_cached_key(|path| path.to_string_lossy().to_lowercase());
    if inputs.is_empty() {
        bail!("input directory contains no immediate *.fwob files");
    }

    std::fs::create_dir_all(output)
        .with_context(|| format!("failed to create output directory {}", output.display()))?;
    let mut jobs = Vec::with_capacity(inputs.len());
    for source in inputs {
        let destination = output.join(
            source
                .file_name()
                .context("discovered conversion input has no filename")?,
        );
        ensure_distinct_paths(&source, &destination)?;
        jobs.push(ConversionJob {
            input: source,
            output: destination,
        });
    }
    Ok(jobs)
}

fn has_fwob_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("fwob"))
}

fn ensure_distinct_paths(input: &Path, output: &Path) -> Result<()> {
    let input = std::fs::canonicalize(input)?;
    let output = if output.exists() {
        std::fs::canonicalize(output)?
    } else {
        let parent = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = std::fs::canonicalize(parent).with_context(|| {
            format!(
                "failed to resolve output parent directory {}",
                parent.display()
            )
        })?;
        parent.join(
            output
                .file_name()
                .context("conversion output has no filename")?,
        )
    };
    if input == output {
        bail!(
            "conversion input and output must be different: {}",
            input.display()
        );
    }
    Ok(())
}

fn run_conversion_jobs(
    jobs: &[ConversionJob],
    parallelism: usize,
    target_format: TargetFormat,
    key_field_index: usize,
    page_size: u32,
    write: V2WriteOptions,
) -> Result<()> {
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..parallelism {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(job) = jobs.get(index) else {
                    break;
                };
                if let Err(error) = convert_one(
                    &job.input,
                    &job.output,
                    target_format,
                    key_field_index,
                    page_size,
                    write,
                    parallelism,
                )
                .with_context(|| format!("failed to convert {}", job.input.display()))
                {
                    failures
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push((index, format!("{error:#}")));
                }
            });
        }
    });

    let mut failures = failures
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if failures.is_empty() {
        return Ok(());
    }
    failures.sort_by_key(|(index, _)| *index);
    let details = failures
        .iter()
        .map(|(_, error)| format!("  {error}"))
        .collect::<Vec<_>>()
        .join("\n");
    bail!("{} conversion job(s) failed:\n{details}", failures.len())
}

fn convert_one(
    input: &Path,
    output: &Path,
    target_format: TargetFormat,
    key_field_index: usize,
    page_size: u32,
    write: V2WriteOptions,
    parallelism: usize,
) -> Result<()> {
    // Detect the source format so any source can be converted to either target, including v2
    // repacking with different page options.
    let source_format = detect_format(input)?;
    let meta = read_source_meta(source_format, input, key_field_index)?;
    match target_format {
        TargetFormat::V2 => convert_to_v2(
            source_format,
            input,
            output,
            key_field_index,
            page_size,
            write,
            meta,
            parallelism,
        ),
        TargetFormat::V1 => convert_to_v1(
            source_format,
            input,
            output,
            key_field_index,
            meta,
            parallelism,
        ),
    }
}

struct SourceMeta {
    schema: fwob_core::Schema,
    title: String,
    string_table: Vec<String>,
    frame_count: u64,
    frame_len: usize,
    page_count: u64,
}

fn read_source_meta(format: Format, input: &Path, key_field_index: usize) -> Result<SourceMeta> {
    match format {
        Format::V1 => {
            let mut reader = fwob_v1::Reader::open(input, key_field_index)
                .with_context(|| format!("failed to open v1 file {}", input.display()))?;
            let string_table = reader.read_string_table()?;
            Ok(SourceMeta {
                schema: reader.schema().clone(),
                title: reader.header().title.clone(),
                string_table,
                frame_count: reader.header().frame_count,
                frame_len: reader.header().frame_length as usize,
                page_count: 0,
            })
        }
        Format::V2 => {
            let reader = fwob_v2::Reader::open(input)
                .with_context(|| format!("failed to open v2 file {}", input.display()))?;
            let header = reader.header().clone();
            Ok(SourceMeta {
                schema: header.schema.clone(),
                title: header.title.clone(),
                string_table: header.string_table.clone(),
                frame_count: header.frame_count,
                frame_len: header.schema.frame_len as usize,
                page_count: header.page_count,
            })
        }
    }
}

/// Streams raw frame bytes from any source version into `sink`, batched by v1 chunk or v2 page,
/// with periodic progress logging. Copies raw bytes only — no per-frame decode/allocation — so a
/// single generic driver is as efficient as the version-specific ones it replaces.
fn stream_source_raw<F>(
    format: Format,
    input: &Path,
    key_field_index: usize,
    chunk_frames: usize,
    frame_len: usize,
    total_frames: u64,
    mut sink: F,
) -> Result<()>
where
    F: FnMut(Vec<u8>) -> Result<()>,
{
    let started = std::time::Instant::now();
    let input_label = input
        .file_name()
        .unwrap_or(input.as_os_str())
        .to_string_lossy();
    let progress_step = 5_000_000u64;
    let mut next_progress = progress_step;
    let mut report = |converted: u64| {
        if converted >= next_progress || converted == total_frames {
            log_info(format!(
                "converted {}: {}/{} frames ({:.1}%) in {:.1}s",
                input_label,
                comma_u64(converted),
                comma_u64(total_frames),
                if total_frames == 0 {
                    100.0
                } else {
                    converted as f64 * 100.0 / total_frames as f64
                },
                started.elapsed().as_secs_f64()
            ));
            while next_progress <= converted {
                next_progress += progress_step;
            }
        }
    };

    match format {
        Format::V1 => {
            let mut reader = fwob_v1::Reader::open(input, key_field_index)?;
            let chunk_frames = chunk_frames.max(1);
            let mut frame_index = 0u64;
            while frame_index < total_frames {
                let raw = reader.read_raw_frames_chunk(frame_index, chunk_frames)?;
                if raw.is_empty() {
                    break;
                }
                let frames_read = (raw.len() / frame_len) as u64;
                sink(raw)?;
                frame_index += frames_read;
                report(frame_index);
            }
        }
        Format::V2 => {
            let mut reader = fwob_v2::Reader::open(input)?;
            let page_count = reader.header().page_count;
            let mut converted = 0u64;
            for page_index in 0..page_count {
                let raw = reader.read_page_raw_frames(page_index)?;
                converted += (raw.len() / frame_len) as u64;
                sink(raw)?;
                report(converted);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn convert_to_v2(
    source_format: Format,
    input: &Path,
    output: &Path,
    key_field_index: usize,
    page_size: u32,
    write: V2WriteOptions,
    meta: SourceMeta,
    parallelism: usize,
) -> Result<()> {
    let mut options = fwob_v2::WriterOptions::new(meta.title.clone());
    options.page_size = page_size;
    options.codec = write.codec.codec();
    options.codec_selection = write.codec.selection();
    options.zstd_level = write.zstd_level;
    options.encoding = write.encoding.encoding();
    options.encoding_selection = write.encoding.selection();
    options.string_table = meta.string_table.clone();
    options.compress_partial_page = write.compress_partial_page;
    options.page_packing = write.page_packing.page_packing();

    let mut writer = fwob_v2::Writer::create(output, meta.schema.clone(), options)
        .with_context(|| format!("failed to create v2 file {}", output.display()))?;

    let chunk_frames = bench::v1_conversion_chunk_frames(
        write.codec.codec(),
        write.encoding.selection(),
        page_size,
        &meta.schema,
    );
    let started = std::time::Instant::now();
    stream_source_raw(
        source_format,
        input,
        key_field_index,
        chunk_frames,
        meta.frame_len,
        meta.frame_count,
        |raw| {
            writer.append_presorted_raw_frames_owned(raw)?;
            Ok(())
        },
    )?;
    let packing_stats = writer.finish_with_stats()?;

    let mut inspect = fwob_v2::Reader::open(output)?;
    if write.verify {
        inspect.verify()?;
    }
    let metadata = collect_v2_metadata(output, &mut inspect)?;
    let _output_guard = CONVERSION_OUTPUT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    print_convert_v2_toml(
        input,
        output,
        key_field_index,
        page_size,
        write,
        inspect.header().frame_count,
        inspect.header().page_count,
        packing_stats,
        &metadata,
        write.verify,
        started.elapsed().as_secs_f64(),
        parallelism,
    );
    Ok(())
}

fn convert_to_v1(
    source_format: Format,
    input: &Path,
    output: &Path,
    key_field_index: usize,
    meta: SourceMeta,
    parallelism: usize,
) -> Result<()> {
    let mut options = fwob_v1::WriterOptions::new(meta.title.clone());
    let estimated_string_bytes: usize = meta.string_table.iter().map(|s| s.len() + 5).sum();
    options.string_table_preserved_length = estimated_string_bytes.max(1834) as u32;
    let mut writer = fwob_v1::Writer::create(output, meta.schema.clone(), options)
        .with_context(|| format!("failed to create v1 file {}", output.display()))?;
    for value in &meta.string_table {
        writer.append_string(value)?;
    }

    let chunk_frames = (4 * 1024 * 1024 / meta.frame_len.max(1)).max(1);
    let started = std::time::Instant::now();
    stream_source_raw(
        source_format,
        input,
        key_field_index,
        chunk_frames,
        meta.frame_len,
        meta.frame_count,
        |raw| {
            writer.append_presorted_raw_frames(&raw)?;
            Ok(())
        },
    )?;
    drop(writer);

    let _output_guard = CONVERSION_OUTPUT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    toml_section("conversion");
    toml_kv_str("target_format", "fwob-v1");
    toml_kv_str("input", &input.display().to_string());
    toml_kv_str("output", &output.display().to_string());
    toml_kv_num("frames", meta.frame_count);
    toml_kv_num("parallelism", parallelism);
    if matches!(source_format, Format::V2) {
        toml_kv_num("source_pages", meta.page_count);
    }
    toml_kv_num(
        "elapsed_seconds",
        format!("{:.3}", started.elapsed().as_secs_f64()),
    );
    Ok(())
}

pub(super) fn append_to_v2(args: AppendArgs) -> Result<()> {
    let parsed = parse_command_tokens(&args.target, false, true, false, false, false)?;
    let write = parsed.write_options(args.write);
    if parsed.paths.len() < 2 {
        bail!("append expects a target and at least one input after tokens");
    }
    let target = PathBuf::from(parsed.paths[0]);
    let inputs: Vec<PathBuf> = parsed.paths[1..].iter().map(PathBuf::from).collect();

    validate_zstd_level(write.zstd_level)?;
    let target_canonical = std::fs::canonicalize(&target).ok();
    for input in &inputs {
        if target_canonical.is_some() && std::fs::canonicalize(input).ok() == target_canonical {
            bail!("target and input must be different files");
        }
    }

    // Append into the target in whatever format it already is; inputs may be v1 or v2 and are
    // appended in the given order.
    match detect_format(&target)? {
        Format::V2 => append_into_v2_target(&target, &inputs, args.key_field_index, write),
        Format::V1 => append_into_v1_target(&target, &inputs, args.key_field_index),
    }
}

fn append_into_v2_target(
    target: &Path,
    inputs: &[PathBuf],
    key_field_index: usize,
    write: V2WriteOptions,
) -> Result<()> {
    let target_header = fwob_v2::Reader::open(target)
        .with_context(|| format!("failed to open target v2 file {}", target.display()))?
        .header()
        .clone();
    let mut options = fwob_v2::WriterOptions::new(target_header.title.clone());
    options.page_size = target_header.page_size;
    options.codec = write.codec.codec();
    options.codec_selection = write.codec.selection();
    options.zstd_level = write.zstd_level;
    options.encoding = write.encoding.encoding();
    options.encoding_selection = write.encoding.selection();
    options.string_table = target_header.string_table.clone();
    options.compress_partial_page = write.compress_partial_page;
    options.page_packing = write.page_packing.page_packing();

    let mut writer = fwob_v2::Writer::open_append(target, options)
        .with_context(|| format!("failed to open target for append {}", target.display()))?;

    for input in inputs {
        match detect_format(input)? {
            Format::V1 => {
                append_v1_input(input, key_field_index, write, &target_header, &mut writer)
            }
            Format::V2 => append_v2_input(input, &target_header, &mut writer),
        }
        .with_context(|| format!("failed while appending input {}", input.display()))?;
    }

    let packing_stats = writer.finish_with_stats()?;

    let mut inspect = fwob_v2::Reader::open(target)?;
    if write.verify {
        inspect.verify()?;
    }
    let metadata = collect_v2_metadata(target, &mut inspect)?;
    toml_section("append");
    toml_kv_str("output", &target.display().to_string());
    toml_kv_num("inputs", inputs.len());
    toml_kv_num("frames", inspect.header().frame_count);
    toml_kv_num("pages", inspect.header().page_count);
    toml_kv_bool("verified", write.verify);
    if !write.verify {
        toml_kv_str("verification", "skipped (run `fwob verify` or pass verify)");
    }
    println!();
    print_packing_stats_toml(packing_stats);

    println!();
    print_compression_stats_toml(&metadata);

    println!();
    toml_section("page_stats");
    print_page_codec_encoding_stats_toml(&metadata);
    Ok(())
}

fn append_into_v1_target(target: &Path, inputs: &[PathBuf], key_field_index: usize) -> Result<()> {
    // v1 cannot persist semantics, so compare structurally.
    let (target_schema, target_strings) = {
        let mut reader = fwob_v1::Reader::open(target, key_field_index)
            .with_context(|| format!("failed to open target v1 file {}", target.display()))?;
        let strings = reader.read_string_table()?;
        (reader.schema().clone(), strings)
    };

    let mut writer = fwob_v1::Writer::open_append(target, key_field_index)
        .with_context(|| format!("failed to open target for append {}", target.display()))?;
    for input in inputs {
        let input_format = detect_format(input)?;
        let input_meta = read_source_meta(input_format, input, key_field_index)?;
        if !input_meta.schema.is_compatible(&target_schema) {
            bail!("input schema does not match target schema");
        }
        if input_meta.string_table != target_strings {
            bail!("input string table does not match target string table");
        }
        let chunk_frames = (4 * 1024 * 1024 / input_meta.frame_len.max(1)).max(1);
        // The v1 writer enforces key order at each chunk boundary, so out-of-order input is rejected.
        stream_source_raw(
            input_format,
            input,
            key_field_index,
            chunk_frames,
            input_meta.frame_len,
            input_meta.frame_count,
            |raw| {
                writer.append_presorted_raw_frames(&raw)?;
                Ok(())
            },
        )
        .with_context(|| format!("failed while appending input {}", input.display()))?;
    }
    let frame_count = writer.frame_count();
    drop(writer);

    toml_section("append");
    toml_kv_str("output", &target.display().to_string());
    toml_kv_num("inputs", inputs.len());
    toml_kv_num("frames", frame_count);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn print_convert_v2_toml(
    input: &Path,
    output: &Path,
    key_field_index: usize,
    page_size: u32,
    write: V2WriteOptions,
    frame_count: u64,
    page_count: u64,
    packing_stats: fwob_v2::PackingStats,
    metadata: &V2Metadata,
    verified: bool,
    elapsed_seconds: f64,
    parallelism: usize,
) {
    toml_section("conversion");
    toml_kv_str("target_format", "fwob-v2");
    toml_kv_str("input", &input.display().to_string());
    toml_kv_str("output", &output.display().to_string());
    toml_kv_num("frames", frame_count);
    toml_kv_num("pages", page_count);
    toml_kv_bool("verified", verified);
    if !verified {
        toml_kv_str("verification", "skipped (run `fwob verify` or pass verify)");
    }
    toml_kv_num("elapsed_seconds", format!("{elapsed_seconds:.3}"));

    println!();
    toml_section("parameters");
    toml_kv_str("target_format", "fwob-v2");
    toml_kv_num("key_field_index", key_field_index);
    toml_kv_num("page_size", page_size);
    toml_kv_num("parallelism", parallelism);
    toml_kv_str("codec", write.codec.as_str());
    toml_kv_str("encoding", write.encoding.as_str());
    toml_kv_num("zstd_level", write.zstd_level);
    toml_kv_bool("compress_partial_page", write.compress_partial_page);
    toml_kv_str("page_packing", write.page_packing.as_str());
    toml_kv_bool("verify", write.verify);

    println!();
    print_packing_stats_toml(packing_stats);

    println!();
    print_compression_stats_toml(metadata);

    println!();
    toml_section("page_stats");
    print_page_codec_encoding_stats_toml(metadata);
}

fn print_packing_stats_toml(stats: fwob_v2::PackingStats) {
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

fn print_compression_stats_toml(metadata: &V2Metadata) {
    toml_section("compression");
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

fn append_v1_input(
    input_path: &Path,
    key_field_index: usize,
    write: V2WriteOptions,
    target_header: &fwob_v2::FileHeader,
    writer: &mut fwob_v2::Writer<std::fs::File>,
) -> Result<()> {
    let mut input = fwob_v1::Reader::open(input_path, key_field_index)
        .with_context(|| format!("failed to open input v1 file {}", input_path.display()))?;
    // The input is v1, which cannot persist field semantics (they read back as None), so compare
    // structurally and ignore semantics. v2->v2 appends (append_v2_input) keep exact equality.
    if !input.schema().is_compatible(&target_header.schema) {
        bail!("input schema does not match target schema");
    }
    let input_strings = input.read_string_table()?;
    if input_strings != target_header.string_table {
        bail!("input string table does not match target string table");
    }

    let frame_len = input.header().frame_length as usize;
    let chunk_frames = bench::v1_conversion_chunk_frames(
        write.codec.codec(),
        write.encoding.selection(),
        target_header.page_size,
        input.schema(),
    );
    let mut frame_index = 0u64;
    while frame_index < input.header().frame_count {
        let chunk = input.read_raw_frames_chunk(frame_index, chunk_frames)?;
        let frames_read = (chunk.len() / frame_len) as u64;
        writer.append_presorted_raw_frames_owned(chunk)?;
        frame_index += frames_read;
    }
    Ok(())
}

fn append_v2_input(
    input_path: &Path,
    target_header: &fwob_v2::FileHeader,
    writer: &mut fwob_v2::Writer<std::fs::File>,
) -> Result<()> {
    let mut input = fwob_v2::Reader::open(input_path)
        .with_context(|| format!("failed to open input v2 file {}", input_path.display()))?;
    if input.header().schema != target_header.schema {
        bail!("input schema does not match target schema");
    }
    if input.header().string_table != target_header.string_table {
        bail!("input string table does not match target string table");
    }
    for frame in input.read_all_frames()? {
        writer.append_frame(frame.bytes())?;
    }
    Ok(())
}
