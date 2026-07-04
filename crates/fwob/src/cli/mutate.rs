use super::*;

pub(super) fn delete_frames(args: DeleteArgs) -> Result<()> {
    let started = std::time::Instant::now();
    let parsed = parse_command_tokens(&args.target, false, true, false, false, true)?;
    let deletion_packing = parsed
        .deletion_packing
        .unwrap_or(DeletionPackingArg::LocalRepack);
    let Some((path, selector_values)) = parsed.paths.split_first() else {
        bail!("delete expects a PATH");
    };
    // Omitting selectors deletes every frame (all-by-default); explicit selectors filter.
    let deletes_all = selector_values.is_empty();
    let path = PathBuf::from(path);
    let (operation_options, write) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        deletion_packing.deletion_packing(),
        matches!(deletion_packing, DeletionPackingArg::LocalRepack),
    );
    let reader_options = operation_options.reader_options;
    let mut reader = fwob::Reader::open_with_options(&path, reader_options)?;
    let total_frames = reader.frame_count();
    let resolved = resolve_selectors(&mut reader, selector_values.iter().copied())?;
    let ranges = resolved.selection.ranges().to_vec();
    let selected_frames = resolved.selection.frame_count();
    drop(reader);

    // Confirm before touching the file when the operation would actually remove frames.
    if selected_frames > 0 {
        let scope = if deletes_all {
            "all frames"
        } else {
            "the selected frames"
        };
        let summary = vec![format!(
            "About to delete {} of {} frame(s) from {} ({} will remain).",
            comma_u64(selected_frames),
            comma_u64(total_frames),
            path.display(),
            comma_u64(total_frames - selected_frames),
        )];
        let question = format!("Delete {scope} from {}?", path.display());
        if !confirm_destructive(&summary, &question, args.yes)? {
            log_info("deletion aborted");
            return Ok(());
        }
    }

    log_info(format!(
        "deletion started: {} selectors={} frames={}",
        path.display(),
        resolved.selector_count,
        selected_frames
    ));
    let progress = ProgressTicker::start("deletion");
    let effective_compress_partial_page =
        matches!(deletion_packing, DeletionPackingArg::LocalRepack) || write.compress_partial_page;
    let mut editor = fwob::Editor::open_with_operation_options(&path, operation_options)?;
    let removed = editor.delete_ranges(&ranges)?;
    if write.verify {
        fwob::Maintenance::verify(&path, reader_options)?;
    }
    let remaining_frames = editor.frame_count();
    drop(editor);
    let storage = StorageSummary::collect(std::slice::from_ref(&path), args.key_field_index)?;
    let page_size = storage
        .v2_metadata()
        .map(|_| fwob_v2::Reader::open(&path).map(|reader| reader.header().page_size))
        .transpose()?;
    drop(progress);
    log_info(format!("deletion completed: {}", path.display()));

    print_operation_result(OperationResult {
        section: "deletion",
        storage: &storage,
        input: None,
        output: &path,
        input_count: 1,
        verified: write.verify,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    });
    toml_kv_num("selector_count", resolved.selector_count);
    toml_kv_num("range_count", ranges.len());
    toml_kv_num("deleted_frames", removed);
    debug_assert_eq!(storage.frame_count(), remaining_frames);
    toml_kv_str("deletion_packing", deletion_packing.as_str());
    toml_kv_bool("compress_partial_page", effective_compress_partial_page);
    print_common_sections(CommonSummary {
        storage: &storage,
        key_field_index: args.key_field_index,
        page_size,
        write: storage.v2_metadata().map(|_| write),
        packing: None,
        parallelism: None,
        verified: write.verify,
    });
    Ok(())
}

pub(super) fn split_file(args: SplitArgs) -> Result<()> {
    use fwob::{Organizer, Reader};
    let started = std::time::Instant::now();

    let parsed = parse_command_tokens(&args.target, false, true, true, false, false)?;
    if parsed.paths.len() < 3 {
        bail!("split expects INPUT OUTPUT_DIR and at least one FIRST_KEY after tokens");
    }
    let input = PathBuf::from(parsed.paths[0]);
    let output_dir = PathBuf::from(parsed.paths[1]);
    let (operation_options, write) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        fwob::DeletionPacking::LocalRepack,
        false,
    );
    let reader_options = operation_options.reader_options;
    let reader = Reader::open_with_options(&input, reader_options)?;
    let source_format = reader.format_version();
    let key_type = fwob_core::KeyType::from_field(reader.schema().key_field())?;
    let keys = parsed.paths[2..]
        .iter()
        .map(|value| Key::parse(key_type, value).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    drop(reader);
    log_info(format!(
        "split started: {} boundaries={}",
        input.display(),
        keys.len()
    ));
    let progress = ProgressTicker::start("split");
    let outputs = Organizer {
        operation_options,
        keep_empty_parts: args.keep_empty_parts,
        output_page_size: Some(parsed.page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE)),
        ..Default::default()
    }
    .split(&input, &output_dir, &keys)?;
    if write.verify {
        for output in &outputs {
            fwob::Maintenance::verify(output, reader_options)?;
        }
    }
    let output_page_size = parsed.page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE);
    let storage = if outputs.is_empty() {
        StorageSummary::empty(source_format, output_page_size)
    } else {
        StorageSummary::collect(&outputs, args.key_field_index)?
    };
    drop(progress);
    log_info(format!(
        "split completed: {} parts={}",
        input.display(),
        outputs.len()
    ));
    print_operation_result(OperationResult {
        section: "split",
        storage: &storage,
        input: Some(&input),
        output: &output_dir,
        input_count: 1,
        verified: write.verify,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    });
    toml_kv_num("parts", outputs.len());
    for (index, path) in outputs.iter().enumerate() {
        toml_kv_str(&format!("part_{index}"), &path.display().to_string());
    }
    print_common_sections(CommonSummary {
        storage: &storage,
        key_field_index: args.key_field_index,
        page_size: storage.v2_metadata().map(|_| output_page_size),
        write: storage.v2_metadata().map(|_| write),
        packing: None,
        parallelism: None,
        verified: write.verify,
    });
    Ok(())
}

pub(super) fn concat_file(args: ConcatArgs) -> Result<()> {
    let started = std::time::Instant::now();
    // concat creates a new file, so (like create/convert) it accepts an output format token plus
    // v2 write tokens and a page-size token.
    let parsed = parse_command_tokens(&args.target, true, true, true, false, false)?;
    if parsed.paths.len() < 2 {
        bail!("concat expects OUTPUT and at least one INPUT after tokens");
    }
    if matches!(parsed.format, Some(TargetFormat::V1)) && parsed.has_v2_write_tokens() {
        bail!("v2 write tokens are not valid when concatenating to v1");
    }
    let output = PathBuf::from(parsed.paths[0]);
    ensure_output_available(&output, args.force)?;
    let inputs = parsed.paths[1..]
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let relaxed_semantics = concat_uses_relaxed_semantics(&inputs)?;
    let target_format = parsed.format.unwrap_or(DEFAULT_TARGET_FORMAT);
    let output_format = Some(match target_format {
        TargetFormat::V1 => fwob_core::FormatVersion::V1,
        TargetFormat::V2 => fwob_core::FormatVersion::V2,
    });
    let output_page_size = Some(parsed.page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE));
    let (operation_options, write) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        fwob::DeletionPacking::LocalRepack,
        false,
    );
    let reader_options = operation_options.reader_options;
    log_info(format!(
        "concat started: output={} inputs={}",
        output.display(),
        inputs.len()
    ));
    let progress = ProgressTicker::start("concat");
    let frames = fwob::Organizer {
        operation_options,
        output_format,
        output_page_size,
        ..Default::default()
    }
    .concat(&output, &inputs)?;
    if write.verify {
        fwob::Maintenance::verify(&output, reader_options)?;
    }
    if relaxed_semantics {
        if matches!(target_format, TargetFormat::V1) {
            log_warn(
                "warning: mixed v1/v2 concat ignored missing v1 field semantics; v2 semantics were dropped for v1 output",
            );
        } else {
            log_warn(
                "warning: mixed v1/v2 concat ignored missing v1 field semantics; v2 semantics were preserved in v2 output",
            );
        }
    }
    let storage = StorageSummary::collect(std::slice::from_ref(&output), args.key_field_index)?;
    drop(progress);
    log_info(format!("concat completed: {}", output.display()));
    debug_assert_eq!(storage.frame_count(), frames);
    print_operation_result(OperationResult {
        section: "concat",
        storage: &storage,
        input: None,
        output: &output,
        input_count: inputs.len(),
        verified: write.verify,
        elapsed_seconds: started.elapsed().as_secs_f64(),
    });
    print_common_sections(CommonSummary {
        storage: &storage,
        key_field_index: args.key_field_index,
        page_size: storage
            .v2_metadata()
            .map(|_| parsed.page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE)),
        write: storage.v2_metadata().map(|_| write),
        packing: None,
        parallelism: None,
        verified: write.verify,
    });
    Ok(())
}

fn concat_uses_relaxed_semantics(inputs: &[PathBuf]) -> Result<bool> {
    let mut has_v1 = false;
    let mut has_semantic_v2 = false;
    for input in inputs {
        match detect_format(input)? {
            Format::V1 => has_v1 = true,
            Format::V2 => {
                let reader = fwob_v2::Reader::open(input)?;
                has_semantic_v2 |= reader
                    .header()
                    .schema
                    .fields
                    .iter()
                    .any(|field| field.semantic != fwob_core::FieldSemantic::None);
            }
        }
    }
    Ok(has_v1 && has_semantic_v2)
}

pub(super) fn edit_file(args: EditArgs) -> Result<()> {
    let edits_metadata = args.title.is_some()
        || args.frame_type.is_some()
        || !args.append_strings.is_empty()
        || args.clear_strings;
    if !edits_metadata && args.set_semantics.is_empty() {
        bail!(
            "edit requires --title, --frame-type, --append-string, --clear-strings, or --set-semantic"
        );
    }

    let mut paths: Vec<PathBuf> = args.target.iter().map(PathBuf::from).collect();
    if paths.is_empty() {
        paths.push(PathBuf::from("."));
    }
    let files = super::discovery::discover_files(&paths)?;
    if files.is_empty() {
        bail!("no .fwob files found to edit");
    }

    // Confirm once for the whole batch, listing the files and the metadata changes to apply.
    let mut changes = Vec::new();
    if let Some(title) = &args.title {
        changes.push(format!("title={title:?}"));
    }
    if let Some(frame_type) = &args.frame_type {
        changes.push(format!("frame-type={frame_type:?}"));
    }
    if args.clear_strings {
        changes.push("clear string table".to_owned());
    }
    if !args.append_strings.is_empty() {
        changes.push(format!("append {} string(s)", args.append_strings.len()));
    }
    for update in &args.set_semantics {
        changes.push(format!("set-semantic {update}"));
    }
    let mut summary = vec![format!(
        "About to rewrite {} file(s) [{}]:",
        files.len(),
        changes.join(", ")
    )];
    for file in &files {
        summary.push(format!("  {}", file.display()));
    }
    let question = format!("Apply this edit to {} file(s)?", files.len());
    if !confirm_destructive(&summary, &question, args.yes)? {
        log_info("edit aborted");
        return Ok(());
    }

    // Apply the same edit to every discovered file, reporting and skipping any that fail so one
    // bad file does not abort the batch. A non-zero exit still signals partial failure.
    let mut failures = 0usize;
    for path in &files {
        if let Err(error) = edit_one_file(path, &args, edits_metadata) {
            log_error(&error.context(format!("failed to edit {}", path.display())));
            failures += 1;
        }
    }
    if failures > 0 {
        bail!("{failures} of {} file(s) could not be edited", files.len());
    }
    Ok(())
}

fn edit_one_file(path: &Path, args: &EditArgs, edits_metadata: bool) -> Result<()> {
    use fwob::Editor;

    // Validate every semantic edit before applying any metadata change. This keeps deterministic
    // validation failures from partially applying a combined edit command.
    let semantic_updates = if args.set_semantics.is_empty() {
        None
    } else {
        match detect_format(path)? {
            Format::V1 => bail!("v1 files cannot store field semantics; convert to v2 first"),
            Format::V2 => {
                let schema = fwob_v2::Reader::open(path)?.header().schema.clone();
                let updates = parse_semantic_updates(&args.set_semantics, &schema)?;
                validate_semantic_updates(&schema, &updates)?;
                Some(updates)
            }
        }
    };

    // Title / frame-type / string-table edits go through the version-neutral editor.
    if edits_metadata {
        let mut editor = Editor::open_with_options(
            path,
            fwob::ReaderOptions {
                v1_key_field_index: args.key_field_index,
            },
        )?;
        let strings = if args.clear_strings || !args.append_strings.is_empty() {
            let mut values = if args.clear_strings {
                Vec::new()
            } else {
                editor.string_table().to_vec()
            };
            values.extend(args.append_strings.clone());
            Some(values)
        } else {
            None
        };
        editor.update_metadata(
            args.frame_type.as_deref(),
            args.title.as_deref(),
            strings.as_deref(),
        )?;
    }

    if let Some(updates) = semantic_updates {
        fwob_v2::update_field_semantics(path, &updates)?;
    }

    let reader = fwob::Reader::open(path)?;
    toml_array_section("edit");
    toml_kv_str("path", &path.display().to_string());
    toml_kv_str("title", reader.title());
    toml_kv_str("frame_type", &reader.schema().frame_type);
    toml_kv_num("string_count", reader.string_table().len());
    for field in &reader.schema().fields {
        if !matches!(field.semantic, fwob_core::FieldSemantic::None) {
            toml_kv_str(
                &format!("semantic.{}", field.name),
                inspect::field_semantic_name(field.semantic),
            );
        }
    }
    Ok(())
}

fn validate_semantic_updates(
    schema: &Schema,
    updates: &[(String, fwob_core::FieldSemantic)],
) -> Result<()> {
    let mut fields = schema.fields.clone();
    for (name, semantic) in updates {
        let field = fields
            .iter_mut()
            .find(|field| &field.name == name)
            .expect("field names were validated while parsing semantic updates");
        field.semantic = *semantic;
    }
    Schema::new(schema.frame_type.clone(), fields, schema.key_field_index)?;
    Ok(())
}

/// Parses `NAME=VALUE` semantic edits, validating field names against `schema`.
fn parse_semantic_updates(
    values: &[String],
    schema: &Schema,
) -> Result<Vec<(String, fwob_core::FieldSemantic)>> {
    use fwob_core::{FieldSemantic, TimestampUnit};
    let mut updates = Vec::with_capacity(values.len());
    for value in values {
        let (name, semantic) = value
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("--set-semantic expects NAME=VALUE, got '{value}'"))?;
        if !schema.fields.iter().any(|field| field.name == name) {
            bail!("field '{name}' not found in schema");
        }
        let semantic = match semantic {
            "none" => FieldSemantic::None,
            "unix-seconds" => FieldSemantic::UnixTimestamp(TimestampUnit::Seconds),
            "unix-milliseconds" => FieldSemantic::UnixTimestamp(TimestampUnit::Milliseconds),
            "unix-microseconds" => FieldSemantic::UnixTimestamp(TimestampUnit::Microseconds),
            "unix-nanoseconds" => FieldSemantic::UnixTimestamp(TimestampUnit::Nanoseconds),
            other => {
                if let Some(points) = parse_decimal_points(other, "fixed-") {
                    FieldSemantic::FixedPoint(points)
                } else if let Some(points) = parse_decimal_points(other, "percent-") {
                    FieldSemantic::Percentage(points)
                } else {
                    bail!(
                        "unknown semantic '{other}'; expected none, unix-seconds, \
                         unix-milliseconds, unix-microseconds, unix-nanoseconds, \
                         fixed-0..fixed-8, or percent-0..percent-8"
                    )
                }
            }
        };
        updates.push((name.to_owned(), semantic));
    }
    Ok(updates)
}

/// Parses `<prefix>N` into the decimal-point count `N`, accepting only `0..=MAX_DECIMAL_POINTS`.
fn parse_decimal_points(value: &str, prefix: &str) -> Option<u8> {
    value
        .strip_prefix(prefix)
        .and_then(|rest| rest.parse::<u8>().ok())
        .filter(|points| *points <= fwob_core::MAX_DECIMAL_POINTS)
}
