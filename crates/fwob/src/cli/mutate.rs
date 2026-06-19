use super::*;

pub(super) fn delete_frames(args: DeleteArgs) -> Result<()> {
    let parsed = parse_command_tokens(&args.target, false, true, false, false, true)?;
    let deletion_packing = parsed
        .deletion_packing
        .unwrap_or(DeletionPackingArg::LocalRepack);
    let ([path, first_key] | [path, first_key, _]) = parsed.paths.as_slice() else {
        bail!("delete expects PATH FIRST_KEY or PATH FIRST_KEY LAST_KEY after tokens");
    };
    let last_key = parsed.paths.get(2).copied();
    let path = PathBuf::from(path);
    let (operation_options, write) = parsed.operation_options(
        args.key_field_index,
        args.zstd_level,
        deletion_packing.deletion_packing(),
        matches!(deletion_packing, DeletionPackingArg::LocalRepack),
    );
    let reader_options = operation_options.reader_options;
    let reader = fwob::Reader::open_with_options(&path, reader_options)?;
    let key_type = fwob_core::KeyType::from_field(reader.schema().key_field())?;
    let first_key = Key::parse(key_type, first_key)?;
    let last_key_value = last_key
        .map(|value| Key::parse(key_type, value))
        .transpose()?
        .unwrap_or(first_key);
    if first_key > last_key_value {
        bail!("FIRST_KEY must be less than or equal to LAST_KEY");
    }
    drop(reader);

    let effective_compress_partial_page =
        matches!(deletion_packing, DeletionPackingArg::LocalRepack) || write.compress_partial_page;
    let mut editor = fwob::Editor::open_with_operation_options(&path, operation_options)?;
    let removed = if first_key == last_key_value {
        editor.delete_key(first_key)?
    } else {
        editor.delete_key_range(first_key..=last_key_value)?
    };
    if write.verify {
        fwob::Maintenance::verify(&path, reader_options)?;
    }

    toml_section("deletion");
    toml_kv_str("path", &path.display().to_string());
    toml_kv_key("first_key", first_key);
    toml_kv_key("last_key", last_key_value);
    toml_kv_num("deleted_frames", removed);
    toml_kv_num("remaining_frames", editor.frame_count());
    toml_kv_str("deletion_packing", deletion_packing.as_str());
    toml_kv_bool("compress_partial_page", effective_compress_partial_page);
    toml_kv_bool("verified", write.verify);
    Ok(())
}

pub(super) fn split_file(args: SplitArgs) -> Result<()> {
    use fwob::{Organizer, Reader};

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
    let key_type = fwob_core::KeyType::from_field(reader.schema().key_field())?;
    let keys = parsed.paths[2..]
        .iter()
        .map(|value| Key::parse(key_type, value).map_err(Into::into))
        .collect::<Result<Vec<_>>>()?;
    drop(reader);
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
    toml_section("split");
    toml_kv_num("parts", outputs.len());
    for (index, path) in outputs.iter().enumerate() {
        toml_kv_str(&format!("part_{index}"), &path.display().to_string());
    }
    Ok(())
}

pub(super) fn concat_file(args: ConcatArgs) -> Result<()> {
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
    if output.exists() && !args.force {
        bail!(
            "output {} already exists; pass --force to overwrite it",
            output.display()
        );
    }
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
    toml_section("concat");
    toml_kv_str("output", &output.display().to_string());
    toml_kv_str(
        "format",
        match target_format {
            TargetFormat::V1 => "fwob-v1",
            TargetFormat::V2 => "fwob-v2",
        },
    );
    toml_kv_num("frames", frames);
    toml_kv_bool("verified", write.verify);
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
    use fwob::Editor;

    let edits_metadata =
        args.title.is_some() || !args.append_strings.is_empty() || args.clear_strings;
    if !edits_metadata && args.set_semantics.is_empty() {
        bail!("edit requires --title, --append-string, --clear-strings, or --set-semantic");
    }

    // Validate every semantic edit before applying any metadata change. This keeps deterministic
    // validation failures from partially applying a combined edit command.
    let semantic_updates = if args.set_semantics.is_empty() {
        None
    } else {
        match detect_format(&args.path)? {
            Format::V1 => bail!("v1 files cannot store field semantics; convert to v2 first"),
            Format::V2 => {
                let schema = fwob_v2::Reader::open(&args.path)?.header().schema.clone();
                let updates = parse_semantic_updates(&args.set_semantics, &schema)?;
                validate_semantic_updates(&schema, &updates)?;
                Some(updates)
            }
        }
    };

    // Title / string-table edits go through the version-neutral editor.
    if edits_metadata {
        let mut editor = Editor::open_with_options(
            &args.path,
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
        editor.update_metadata(args.title.as_deref(), strings.as_deref())?;
    }

    if let Some(updates) = semantic_updates {
        fwob_v2::update_field_semantics(&args.path, &updates)?;
    }

    let reader = fwob::Reader::open(&args.path)?;
    toml_section("edit");
    toml_kv_str("title", reader.title());
    toml_kv_num("string_count", reader.string_table().len());
    if !args.set_semantics.is_empty() {
        for field in &reader.schema().fields {
            if !matches!(field.semantic, fwob_core::FieldSemantic::None) {
                toml_kv_str(
                    &format!("semantic.{}", field.name),
                    inspect::field_semantic_name(field.semantic),
                );
            }
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
            other => bail!(
                "unknown semantic '{other}'; expected none, unix-seconds, unix-milliseconds, \
                 unix-microseconds, or unix-nanoseconds"
            ),
        };
        updates.push((name.to_owned(), semantic));
    }
    Ok(updates)
}
