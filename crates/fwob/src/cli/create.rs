use super::*;

pub(super) fn create_blank(args: NewArgs) -> Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    let (format, output, page_size) = parse_create_target(&args.target)?;
    ensure_output_available(&output, args.force)?;
    let (schema, strings, template_title) = if let Some(template) = &args.template {
        read_template_schema(template, args.key_field_index)?
    } else {
        (
            schema_from_create_args(
                args.frame_type.as_deref(),
                &args.fields,
                args.key_field_index,
            )?,
            Vec::new(),
            None,
        )
    };
    let title = args.title.or(template_title).unwrap_or_else(|| {
        output
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("FWOB")
            .to_string()
    });

    match format {
        TargetFormat::V1 => {
            let mut options = fwob_v1::WriterOptions::new(title);
            let estimated_string_bytes: usize = strings.iter().map(|s| s.len() + 5).sum();
            options.string_table_preserved_length = estimated_string_bytes.max(1834) as u32;
            let mut writer = fwob_v1::Writer::create(&output, schema, options)?;
            for value in &strings {
                writer.append_string(value)?;
            }
        }
        TargetFormat::V2 => {
            let mut options = fwob_v2::WriterOptions::new(title);
            options.page_size = page_size;
            options.string_table = strings;
            fwob_v2::Writer::create(&output, schema, options)?.finish()?;
        }
    }

    w.section("new")?;
    w.kv_str("output", &output.display().to_string())?;
    Ok(())
}

fn parse_create_target(values: &[String]) -> Result<(TargetFormat, PathBuf, u32)> {
    let mut format = None;
    let mut page_size = None;
    let mut paths = Vec::new();
    for value in values {
        if let Some(parsed) = match_target_format(value) {
            set_once(&mut format, parsed, "format")?;
        } else if let Some(parsed) = parse_page_size_token(value) {
            set_once(&mut page_size, parsed?, "page size")?;
        } else if is_any_reserved_token(value) {
            bail!("token '{value}' is not valid for new");
        } else {
            paths.push(value);
        }
    }
    let format = format.unwrap_or(DEFAULT_TARGET_FORMAT);
    if matches!(format, TargetFormat::V1) && page_size.is_some() {
        bail!("page size token is not valid when creating v1");
    }
    match paths.as_slice() {
        [output] => Ok((
            format,
            PathBuf::from(output),
            page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE),
        )),
        [] => bail!("new expects OUTPUT or FORMAT OUTPUT"),
        _ => bail!("new expects exactly one output path"),
    }
}

fn schema_from_create_args(
    frame_type: Option<&str>,
    fields: &[String],
    key_field_index: usize,
) -> Result<Schema> {
    let frame_type = frame_type.context("--frame-type is required when --template is omitted")?;
    if fields.is_empty() {
        bail!("at least one --field is required when --template is omitted");
    }

    let mut offset = 0u32;
    let mut parsed = Vec::with_capacity(fields.len());
    for field in fields {
        let mut parts = field.split(':');
        let name = parts
            .next()
            .filter(|value| !value.is_empty())
            .context("--field must use name:type:length")?;
        let field_type = parts
            .next()
            .map(parse_field_type)
            .transpose()?
            .context("--field must use name:type:length")?;
        let length = parts
            .next()
            .context("--field must use name:type:length")?
            .parse::<u16>()
            .with_context(|| format!("invalid field length in '{field}'"))?;
        if parts.next().is_some() {
            bail!("--field must use name:type:length");
        }
        if length == 0 {
            bail!("field '{name}' length must be greater than zero");
        }
        parsed.push(Field::new(name, field_type, length, offset));
        offset = offset
            .checked_add(u32::from(length))
            .context("schema frame length overflow")?;
    }

    Schema::new(frame_type, parsed, key_field_index).map_err(Into::into)
}

fn parse_field_type(value: &str) -> Result<FieldType> {
    match value {
        "i" | "int" | "signed" | "signed-integer" => Ok(FieldType::SignedInteger),
        "u" | "uint" | "unsigned" | "unsigned-integer" => Ok(FieldType::UnsignedInteger),
        "f" | "float" | "floating" | "floating-point" => Ok(FieldType::FloatingPoint),
        "utf8" | "utf8-string" | "string" => Ok(FieldType::Utf8String),
        "string-index" | "string-table-index" | "stridx" => Ok(FieldType::StringTableIndex),
        _ => bail!("unsupported field type '{value}'"),
    }
}

fn read_template_schema(
    path: &PathBuf,
    v1_key_field_index: usize,
) -> Result<(Schema, Vec<String>, Option<String>)> {
    match detect_format(path)? {
        Format::V1 => {
            let mut reader = fwob_v1::Reader::open(path, v1_key_field_index)?;
            let strings = reader.read_string_table()?;
            Ok((
                reader.schema().clone(),
                strings,
                Some(reader.header().title.clone()),
            ))
        }
        Format::V2 => {
            let reader = fwob_v2::Reader::open(path)?;
            Ok((
                reader.header().schema.clone(),
                reader.header().string_table.clone(),
                Some(reader.header().title.clone()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_type_tokens_are_case_sensitive() {
        assert_eq!(parse_field_type("u").unwrap(), FieldType::UnsignedInteger);
        assert!(parse_field_type("U").is_err());
    }

    #[test]
    fn create_defaults_to_v2() {
        assert!(matches!(DEFAULT_TARGET_FORMAT, TargetFormat::V2));
        assert!(matches!(
            parse_create_target(&["created.fwob".to_owned()]).unwrap().0,
            TargetFormat::V2
        ));
    }
}
