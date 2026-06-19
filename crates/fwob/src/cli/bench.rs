use super::*;

fn resolve_bench_args(args: BenchArgs) -> Result<ResolvedBenchArgs> {
    let (mode, path) = parse_bench_target(&args.target)?;
    Ok(ResolvedBenchArgs {
        path,
        mode,
        iterations: args.iterations,
        first_key_i32: args.first_key_i32,
        last_key_i32: args.last_key_i32,
        key_field_index: args.key_field_index,
        output_dir: args.output_dir,
        scan_iterations: args.scan_iterations,
        keep_outputs: args.keep_outputs,
    })
}

fn parse_bench_target(values: &[String]) -> Result<(BenchMode, PathBuf)> {
    let mut mode = None;
    let mut paths = Vec::new();
    for value in values {
        if let Some(parsed) = match_bench_mode(value) {
            set_once(&mut mode, parsed, "bench mode")?;
        } else if is_any_reserved_token(value) {
            bail!("token '{value}' is not valid for bench");
        } else {
            paths.push(value);
        }
    }
    match paths.as_slice() {
        [path] => Ok((
            mode.unwrap_or(BenchMode::ConversionMatrix),
            PathBuf::from(path),
        )),
        [] => bail!("bench expects PATH or MODE PATH"),
        _ => bail!("bench expects exactly one input path"),
    }
}

pub(super) fn bench_v2(args: BenchArgs) -> Result<()> {
    let args = resolve_bench_args(args)?;
    if matches!(args.mode, BenchMode::ConversionMatrix) {
        return bench_conversion_matrix(args);
    }

    let start = std::time::Instant::now();
    let total_frames = match args.mode {
        BenchMode::ConversionMatrix => unreachable!(),
        BenchMode::Range => bench_range_mode(&args)?,
        BenchMode::RandomPage => bench_random_page_mode(&args)?,
        BenchMode::Scan => bench_scan_mode(&args)?,
    };
    let elapsed = start.elapsed();
    let per_iter = elapsed.as_secs_f64() / args.iterations as f64;
    println!("mode: {}", args.mode.as_str());
    println!("iterations: {}", comma_u64(args.iterations));
    println!("total_frames: {}", comma_usize(total_frames));
    println!(
        "elapsed_ms: {}",
        comma_f64(elapsed.as_secs_f64() * 1000.0, 3)
    );
    println!("avg_iter_us: {}", comma_f64(per_iter * 1_000_000.0, 3));
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ConversionBenchCase {
    page_size_label: &'static str,
    page_size: u32,
    codec: CodecArg,
    zstd_level: Option<i32>,
    encoding: EncodingArg,
    page_packing: PagePackingArg,
}

const CONVERSION_BENCH_PAGE_SIZES: [(&str, u32); 3] = [
    ("512KiB", 512 * 1024),
    ("1MiB", 1024 * 1024),
    ("2MiB", 2 * 1024 * 1024),
];
const CONVERSION_BENCH_CODECS: [CodecArg; 3] =
    [CodecArg::Zstd, CodecArg::Lz4, CodecArg::Uncompressed];
const CONVERSION_BENCH_ZSTD_LEVELS: [i32; 4] = [3, 6, 9, 12];
const CONVERSION_BENCH_ENCODINGS: [EncodingArg; 3] = [
    EncodingArg::RowRaw,
    EncodingArg::ColumnarBasic,
    EncodingArg::ColumnarDelta,
];
const CONVERSION_BENCH_PAGE_PACKINGS: [PagePackingArg; 2] =
    [PagePackingArg::EstimateShrink, PagePackingArg::TightFit];

struct ConversionBenchResult {
    case: ConversionBenchCase,
    elapsed_seconds: f64,
    read_performance: ReadPerformance,
    frame_count: u64,
    page_count: u64,
    physical_bytes: u64,
    compressed_payload_bytes: u64,
    uncompressed_payload_bytes: u64,
    payload_ratio: f64,
    physical_ratio: f64,
    average_frames_per_compressed_page: Option<f64>,
    first_page_attempts: u64,
    subsequent_average_attempts: f64,
    subsequent_min_attempts: u64,
    subsequent_max_attempts: u64,
}

#[derive(Debug, Clone, Copy)]
struct ReadPerformance {
    random_page_iterations: u64,
    random_page_avg_us: f64,
    random_page_frames: u64,
    scan_iterations: u64,
    scan_avg_ms: f64,
    scan_frames: u64,
    range_iterations: u64,
    range_avg_us: f64,
    range_frames: u64,
}

fn bench_conversion_matrix(args: ResolvedBenchArgs) -> Result<()> {
    let cases = conversion_bench_cases();
    let output_dir = args
        .output_dir
        .clone()
        .or_else(|| args.path.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&output_dir)?;

    println!("mode: conversion-matrix");
    println!("input: {}", args.path.display());
    println!("output_dir: {}", output_dir.display());
    println!("cases: {}", comma_usize(cases.len()));
    println!();
    print_conversion_bench_dimensions(&cases, &args);
    println!();

    let mut results = Vec::with_capacity(cases.len());
    for (case_index, case) in cases.iter().copied().enumerate() {
        let output = conversion_bench_output_path(&output_dir, &args.path, case_index, case);
        log_info(format!(
            "bench case {}/{}: page_size={} codec={} zstd_level={} encoding={} page_packing={}",
            comma_usize(case_index + 1),
            comma_usize(cases.len()),
            case.page_size_label,
            case.codec.as_str(),
            case.zstd_level
                .map(|level| level.to_string())
                .unwrap_or_else(|| "-".to_string()),
            case.encoding.as_str(),
            case.page_packing.as_str()
        ));

        log_info("  test=conversion started");
        let conversion_start = std::time::Instant::now();
        let packing_stats = convert_input_to_v2_for_bench(&args, case, &output)
            .with_context(|| format!("benchmark conversion failed for {}", output.display()))?;
        let elapsed_seconds = conversion_start.elapsed().as_secs_f64();
        log_info(format!(
            "  test=conversion completed elapsed_s={}",
            comma_f64(elapsed_seconds, 3)
        ));

        log_info("  test=metadata started");
        let metadata_start = std::time::Instant::now();
        let mut inspect = fwob_v2::Reader::open(&output)?;
        let frame_count = inspect.header().frame_count;
        let page_count = inspect.header().page_count;
        let metadata = collect_v2_metadata(&output, &mut inspect)?;
        drop(inspect);
        log_info(format!(
            "  test=metadata completed elapsed_s={}",
            comma_f64(metadata_start.elapsed().as_secs_f64(), 3)
        ));

        let read_performance =
            measure_v2_read_performance(&output, args.iterations, args.scan_iterations)?;
        let compressed_pages = metadata.codec_zstd_pages + metadata.codec_lz4_pages;
        let result = ConversionBenchResult {
            case,
            elapsed_seconds,
            read_performance,
            frame_count,
            page_count,
            physical_bytes: metadata.physical_bytes,
            compressed_payload_bytes: metadata.compressed_total,
            uncompressed_payload_bytes: metadata.uncompressed_total,
            payload_ratio: ratio(metadata.compressed_total, metadata.uncompressed_total),
            physical_ratio: ratio(metadata.physical_bytes, metadata.uncompressed_total),
            average_frames_per_compressed_page: if compressed_pages == 0 {
                None
            } else {
                Some(metadata.compressed_page_frames as f64 / compressed_pages as f64)
            },
            first_page_attempts: packing_stats.first_page_attempts,
            subsequent_average_attempts: packing_stats.subsequent_average_attempts(),
            subsequent_min_attempts: packing_stats.subsequent_min_attempts,
            subsequent_max_attempts: packing_stats.subsequent_max_attempts,
        };
        println!(
            "case_done: {}/{} physical_bytes={} physical_ratio={:.4} elapsed_s={} random_page_avg_us={} scan_avg_ms={} range_avg_us={}",
            comma_usize(case_index + 1),
            comma_usize(cases.len()),
            comma_u64(result.physical_bytes),
            result.physical_ratio,
            comma_f64(result.elapsed_seconds, 3),
            comma_f64(result.read_performance.random_page_avg_us, 3),
            comma_f64(result.read_performance.scan_avg_ms, 3),
            comma_f64(result.read_performance.range_avg_us, 3)
        );
        results.push(result);

        if !args.keep_outputs {
            std::fs::remove_file(&output).with_context(|| {
                format!("failed to remove benchmark output {}", output.display())
            })?;
        }
    }

    results.sort_by(|left, right| {
        left.physical_bytes
            .cmp(&right.physical_bytes)
            .then_with(|| left.elapsed_seconds.total_cmp(&right.elapsed_seconds))
    });

    println!();
    toml_section("conversion_matrix_summary");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "convert_s",
            "random_ms",
            "scan_ms",
            "range_ms",
            "physical_bytes",
            "ratio",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_f64(result.elapsed_seconds, 3),
                    comma_f64(result.read_performance.random_page_avg_us / 1000.0, 3),
                    comma_f64(result.read_performance.scan_avg_ms, 3),
                    comma_f64(result.read_performance.range_avg_us / 1000.0, 3),
                    comma_u64(result.physical_bytes),
                    format!("{:.4}", result.physical_ratio),
                ]
            })
            .collect(),
        &[
            true, false, false, true, false, false, true, true, true, true, true, true,
        ],
    );

    println!();
    toml_section("conversion_matrix_storage");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "frames",
            "pages",
            "compressed_bytes",
            "uncompressed_bytes",
            "payload_ratio",
            "avg_frames_compressed_page",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_u64(result.frame_count),
                    comma_u64(result.page_count),
                    comma_u64(result.compressed_payload_bytes),
                    comma_u64(result.uncompressed_payload_bytes),
                    format!("{:.4}", result.payload_ratio),
                    result
                        .average_frames_per_compressed_page
                        .map(|average| comma_f64(average, 2))
                        .unwrap_or_else(|| "-".to_string()),
                ]
            })
            .collect(),
        &[
            true, false, false, true, false, false, true, true, true, true, true, true,
        ],
    );

    println!();
    toml_section("conversion_matrix_read_samples");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "random_iterations",
            "random_frames",
            "scan_iterations",
            "scan_frames",
            "range_iterations",
            "range_frames",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_u64(result.read_performance.random_page_iterations),
                    comma_u64(result.read_performance.random_page_frames),
                    comma_u64(result.read_performance.scan_iterations),
                    comma_u64(result.read_performance.scan_frames),
                    comma_u64(result.read_performance.range_iterations),
                    comma_u64(result.read_performance.range_frames),
                ]
            })
            .collect(),
        &[
            true, false, false, true, false, false, true, true, true, true, true, true,
        ],
    );

    println!();
    toml_section("conversion_matrix_packing");
    print_aligned_table(
        &[
            "rank",
            "page",
            "codec",
            "level",
            "encoding",
            "packing",
            "first_attempts",
            "subseq_avg_attempts",
            "subseq_attempt_range",
        ],
        results
            .iter()
            .enumerate()
            .map(|(rank, result)| {
                vec![
                    comma_usize(rank + 1),
                    result.case.page_size_label.to_string(),
                    result.case.codec.as_str().to_string(),
                    result
                        .case
                        .zstd_level
                        .map(|level| level.to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    result.case.encoding.as_str().to_string(),
                    result.case.page_packing.as_str().to_string(),
                    comma_u64(result.first_page_attempts),
                    comma_f64(result.subsequent_average_attempts, 2),
                    format!(
                        "{}..{}",
                        comma_u64(result.subsequent_min_attempts),
                        comma_u64(result.subsequent_max_attempts)
                    ),
                ]
            })
            .collect(),
        &[true, false, false, true, false, false, true, true, true],
    );
    Ok(())
}

fn print_conversion_bench_dimensions(cases: &[ConversionBenchCase], args: &ResolvedBenchArgs) {
    toml_section("conversion_matrix_dimensions");
    println!(
        "page_size ({}): {}",
        CONVERSION_BENCH_PAGE_SIZES.len(),
        CONVERSION_BENCH_PAGE_SIZES
            .iter()
            .map(|(label, _)| format!(
                "{} ({} cases)",
                label,
                cases
                    .iter()
                    .filter(|case| case.page_size_label == *label)
                    .count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "codec ({}): {}",
        CONVERSION_BENCH_CODECS.len(),
        CONVERSION_BENCH_CODECS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value.as_str(),
                cases.iter().filter(|case| case.codec == *value).count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "zstd_level ({}; zstd only): {}",
        CONVERSION_BENCH_ZSTD_LEVELS.len(),
        CONVERSION_BENCH_ZSTD_LEVELS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value,
                cases
                    .iter()
                    .filter(|case| case.zstd_level == Some(*value))
                    .count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "encoding ({}): {}",
        CONVERSION_BENCH_ENCODINGS.len(),
        CONVERSION_BENCH_ENCODINGS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value.as_str(),
                cases.iter().filter(|case| case.encoding == *value).count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!(
        "page_packing ({}): {}",
        CONVERSION_BENCH_PAGE_PACKINGS.len(),
        CONVERSION_BENCH_PAGE_PACKINGS
            .iter()
            .map(|value| format!(
                "{} ({} cases)",
                value.as_str(),
                cases
                    .iter()
                    .filter(|case| case.page_packing == *value)
                    .count()
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("excluded: codec=uncompressed + page_packing=tight-fit");
    println!("conditional: zstd_level applies only to codec=zstd");

    println!();
    toml_section("conversion_matrix_test_runs");
    println!("conversion: {}", comma_usize(cases.len()));
    println!(
        "random_page: {} cases x {} iterations = {} reads",
        comma_usize(cases.len()),
        comma_u64(args.iterations),
        comma_u128(cases.len() as u128 * u128::from(args.iterations))
    );
    println!(
        "scan: {} cases x {} iterations = {} scans",
        comma_usize(cases.len()),
        comma_u64(args.scan_iterations),
        comma_u128(cases.len() as u128 * u128::from(args.scan_iterations))
    );
    println!(
        "range: {} cases x {} iterations = {} queries",
        comma_usize(cases.len()),
        comma_u64(args.iterations),
        comma_u128(cases.len() as u128 * u128::from(args.iterations))
    );
}

fn conversion_bench_cases() -> Vec<ConversionBenchCase> {
    let mut cases = Vec::new();
    for (page_size_label, page_size) in CONVERSION_BENCH_PAGE_SIZES {
        for codec in CONVERSION_BENCH_CODECS {
            for encoding in CONVERSION_BENCH_ENCODINGS {
                for page_packing in CONVERSION_BENCH_PAGE_PACKINGS {
                    if codec == CodecArg::Uncompressed && page_packing == PagePackingArg::TightFit {
                        continue;
                    }
                    if codec == CodecArg::Zstd {
                        for zstd_level in CONVERSION_BENCH_ZSTD_LEVELS {
                            cases.push(ConversionBenchCase {
                                page_size_label,
                                page_size,
                                codec,
                                zstd_level: Some(zstd_level),
                                encoding,
                                page_packing,
                            });
                        }
                    } else {
                        cases.push(ConversionBenchCase {
                            page_size_label,
                            page_size,
                            codec,
                            zstd_level: None,
                            encoding,
                            page_packing,
                        });
                    }
                }
            }
        }
    }
    cases
}

fn conversion_bench_output_path(
    output_dir: &Path,
    input: &Path,
    case_index: usize,
    case: ConversionBenchCase,
) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("fwob");
    let process_id = std::process::id();
    let zstd_level = case
        .zstd_level
        .map(|level| format!("zstd{level}"))
        .unwrap_or_else(|| "nozstd".to_string());
    output_dir.join(format!(
        "{stem}.bench.{process_id}.{case_index}.{}.{}.{}.{}.fwob",
        case.page_size_label,
        case.codec.as_str(),
        zstd_level,
        case.encoding.as_str(),
    ))
}

fn convert_input_to_v2_for_bench(
    args: &ResolvedBenchArgs,
    case: ConversionBenchCase,
    output: &Path,
) -> Result<fwob_v2::PackingStats> {
    match detect_format(&args.path)? {
        Format::V1 => convert_v1_input_to_v2_for_bench(args, case, output),
        Format::V2 => convert_v2_input_to_v2_for_bench(args, case, output),
    }
}

fn bench_writer_options(
    title: String,
    string_table: Vec<String>,
    case: ConversionBenchCase,
) -> fwob_v2::WriterOptions {
    let mut options = fwob_v2::WriterOptions::new(title);
    options.page_size = case.page_size;
    options.codec = case.codec.codec();
    options.codec_selection = case.codec.selection();
    options.zstd_level = case.zstd_level.unwrap_or(fwob_v2::DEFAULT_ZSTD_LEVEL);
    options.encoding = case.encoding.encoding();
    options.encoding_selection = case.encoding.selection();
    options.string_table = string_table;
    options.compress_partial_page = false;
    options.page_packing = case.page_packing.page_packing();
    options
}

pub(super) fn v1_conversion_chunk_frames(
    codec: Codec,
    encoding_selection: EncodingSelection,
    page_size: u32,
    schema: &fwob_core::Schema,
) -> usize {
    fwob_v2::recommended_input_chunk_frames(codec, encoding_selection, page_size, schema)
}

fn convert_v1_input_to_v2_for_bench(
    args: &ResolvedBenchArgs,
    case: ConversionBenchCase,
    output: &Path,
) -> Result<fwob_v2::PackingStats> {
    let mut input = fwob_v1::Reader::open(&args.path, args.key_field_index)
        .with_context(|| format!("failed to open input v1 file {}", args.path.display()))?;
    let strings = input.read_string_table()?;
    let options = bench_writer_options(input.header().title.clone(), strings, case);
    let mut writer = fwob_v2::Writer::create(output, input.schema().clone(), options)
        .with_context(|| format!("failed to create benchmark output {}", output.display()))?;

    let frame_len = input.header().frame_length as usize;
    let chunk_frames = v1_conversion_chunk_frames(
        case.codec.codec(),
        case.encoding.selection(),
        case.page_size,
        input.schema(),
    );
    let mut frame_index = 0u64;
    while frame_index < input.header().frame_count {
        let chunk = input.read_raw_frames_chunk(frame_index, chunk_frames)?;
        writer.append_presorted_raw_frames(&chunk)?;
        frame_index += (chunk.len() / frame_len) as u64;
    }
    writer.finish_with_stats().map_err(Into::into)
}

fn convert_v2_input_to_v2_for_bench(
    args: &ResolvedBenchArgs,
    case: ConversionBenchCase,
    output: &Path,
) -> Result<fwob_v2::PackingStats> {
    let mut input = fwob_v2::Reader::open(&args.path)
        .with_context(|| format!("failed to open input v2 file {}", args.path.display()))?;
    let header = input.header().clone();
    let options = bench_writer_options(header.title.clone(), header.string_table.clone(), case);
    let mut writer = fwob_v2::Writer::create(output, header.schema.clone(), options)
        .with_context(|| format!("failed to create benchmark output {}", output.display()))?;

    for page_index in 0..header.page_count {
        for frame in input.read_page_frames(page_index)? {
            writer.append_frame(frame.bytes())?;
        }
    }
    writer.finish_with_stats().map_err(Into::into)
}

fn measure_v2_read_performance(
    path: &Path,
    iterations: u64,
    scan_iterations: u64,
) -> Result<ReadPerformance> {
    let random_page_iterations = iterations.max(1);
    let mut random_reader = fwob_v2::Reader::open(path)?;
    let page_count = random_reader.header().page_count;
    log_info(format!(
        "  test=random-page started iterations={}",
        comma_u64(random_page_iterations)
    ));
    let random_start = std::time::Instant::now();
    let mut random_page_frames = 0u64;
    if page_count > 0 {
        for i in 0..random_page_iterations {
            let page = (i.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % page_count;
            random_page_frames += random_reader.read_page_frames(page)?.len() as u64;
        }
    }
    let random_page_avg_us =
        random_start.elapsed().as_secs_f64() * 1_000_000.0 / random_page_iterations as f64;
    log_info(format!(
        "  test=random-page completed elapsed_s={} average_us={}",
        comma_f64(random_start.elapsed().as_secs_f64(), 3),
        comma_f64(random_page_avg_us, 3)
    ));

    let scan_iterations = scan_iterations.max(1);
    log_info(format!(
        "  test=scan started iterations={}",
        comma_u64(scan_iterations)
    ));
    let scan_start = std::time::Instant::now();
    let mut scan_frames = 0u64;
    for _ in 0..scan_iterations {
        let mut scan_reader = fwob_v2::Reader::open(path)?;
        scan_frames += scan_reader.read_all_frames()?.len() as u64;
    }
    let scan_avg_ms = scan_start.elapsed().as_secs_f64() * 1000.0 / scan_iterations as f64;
    log_info(format!(
        "  test=scan completed elapsed_s={} average_ms={}",
        comma_f64(scan_start.elapsed().as_secs_f64(), 3),
        comma_f64(scan_avg_ms, 3)
    ));

    let range_iterations = iterations.max(1);
    let mut range_reader = fwob_v2::Reader::open(path)?;
    let range_page = (page_count > 0).then_some(page_count / 2);
    let range_keys = range_page
        .map(|page| range_reader.read_page_header(page))
        .transpose()?
        .map(|header| (header.first_key, header.last_key));
    if let (Some(page), Some((first_key, last_key))) = (range_page, range_keys) {
        log_info(format!(
            "  test=range started iterations={} page={} first_key={} last_key={}",
            comma_u64(range_iterations),
            comma_u64(page),
            toml_key_value(first_key),
            toml_key_value(last_key)
        ));
    } else {
        log_info(format!(
            "  test=range started iterations={} empty_file=true",
            comma_u64(range_iterations)
        ));
    }
    let range_start = std::time::Instant::now();
    let mut range_frames = 0u64;
    if let Some((first_key, last_key)) = range_keys {
        for _ in 0..range_iterations {
            range_frames += range_reader.frames_between(first_key, last_key)?.len() as u64;
        }
    }
    let range_avg_us = range_start.elapsed().as_secs_f64() * 1_000_000.0 / range_iterations as f64;
    log_info(format!(
        "  test=range completed elapsed_s={} average_us={}",
        comma_f64(range_start.elapsed().as_secs_f64(), 3),
        comma_f64(range_avg_us, 3)
    ));

    Ok(ReadPerformance {
        random_page_iterations,
        random_page_avg_us,
        random_page_frames,
        scan_iterations,
        scan_avg_ms,
        scan_frames,
        range_iterations,
        range_avg_us,
        range_frames,
    })
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn bench_range_mode(args: &ResolvedBenchArgs) -> Result<usize> {
    let first = args.first_key_i32.unwrap_or(i32::MIN);
    let last = args.last_key_i32.unwrap_or(i32::MAX);
    let mut total_frames = 0usize;
    for _ in 0..args.iterations {
        let mut reader = fwob_v2::Reader::open(&args.path)?;
        let frames =
            reader.frames_between(fwob_core::Key::I32(first), fwob_core::Key::I32(last))?;
        total_frames += frames.len();
    }
    println!(
        "range: [{}, {}]",
        comma_i128(first as i128),
        comma_i128(last as i128)
    );
    Ok(total_frames)
}

fn bench_random_page_mode(args: &ResolvedBenchArgs) -> Result<usize> {
    let mut reader = fwob_v2::Reader::open(&args.path)?;
    let page_count = reader.header().page_count.max(1);
    let mut total_frames = 0usize;
    for i in 0..args.iterations {
        let page = (i.wrapping_mul(1_103_515_245).wrapping_add(12_345)) % page_count;
        total_frames += reader.read_page_frames(page)?.len();
    }
    Ok(total_frames)
}

fn bench_scan_mode(args: &ResolvedBenchArgs) -> Result<usize> {
    let mut total_frames = 0usize;
    for _ in 0..args.iterations {
        let mut reader = fwob_v2::Reader::open(&args.path)?;
        total_frames += reader.read_all_frames()?.len();
    }
    Ok(total_frames)
}
