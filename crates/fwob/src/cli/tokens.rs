use super::*;

pub(super) fn parse_convert_target(
    values: &[String],
    write_args: V2WriteArgs,
) -> Result<(TargetFormat, PathBuf, PathBuf, u32, V2WriteOptions)> {
    let parsed = parse_command_tokens(values, true, true, true, false, false)?;
    let format = parsed.format.unwrap_or(DEFAULT_TARGET_FORMAT);
    if matches!(format, TargetFormat::V1) && parsed.has_v2_write_tokens() {
        bail!("v2 write tokens are not valid when converting to v1");
    }
    let write = parsed.write_options(write_args);
    match parsed.paths.as_slice() {
        [input, output] => Ok((
            format,
            PathBuf::from(input),
            PathBuf::from(output),
            parsed.page_size.unwrap_or(fwob_v2::DEFAULT_PAGE_SIZE),
            write,
        )),
        _ => bail!("convert expects exactly input and output paths after tokens"),
    }
}

pub(super) fn match_bench_mode(value: &str) -> Option<BenchMode> {
    match value {
        "conversion-matrix" => Some(BenchMode::ConversionMatrix),
        "range" => Some(BenchMode::Range),
        "random-page" => Some(BenchMode::RandomPage),
        "scan" => Some(BenchMode::Scan),
        _ => None,
    }
}

pub(super) fn match_target_format(value: &str) -> Option<TargetFormat> {
    match value {
        "v1" => Some(TargetFormat::V1),
        "v2" => Some(TargetFormat::V2),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct ParsedTokens<'a> {
    pub(super) paths: Vec<&'a str>,
    pub(super) format: Option<TargetFormat>,
    pub(super) codec: Option<CodecArg>,
    pub(super) encoding: Option<EncodingArg>,
    pub(super) page_packing: Option<PagePackingArg>,
    pub(super) deletion_packing: Option<DeletionPackingArg>,
    pub(super) page_size: Option<u32>,
    pub(super) verify: bool,
    pub(super) compress_partial_page: bool,
}

impl ParsedTokens<'_> {
    pub(super) fn has_v2_write_tokens(&self) -> bool {
        self.codec.is_some()
            || self.encoding.is_some()
            || self.page_packing.is_some()
            || self.page_size.is_some()
            || self.verify
            || self.compress_partial_page
    }

    pub(super) fn write_options(&self, args: V2WriteArgs) -> V2WriteOptions {
        let mut write = V2WriteOptions::from_args(args);
        if let Some(codec) = self.codec {
            write.codec = codec;
        }
        if let Some(encoding) = self.encoding {
            write.encoding = encoding;
        }
        if let Some(page_packing) = self.page_packing {
            write.page_packing = page_packing;
        }
        write.verify = self.verify;
        write.compress_partial_page = self.compress_partial_page;
        write
    }

    pub(super) fn operation_options(
        &self,
        v1_key_field_index: usize,
        zstd_level: Option<i32>,
        deletion_packing: fwob::DeletionPacking,
        force_compress_partial_page: bool,
    ) -> (fwob::OperationOptions, V2WriteOptions) {
        let mut write = self.write_options(V2WriteArgs {
            zstd_level: zstd_level.unwrap_or(fwob_v2::DEFAULT_ZSTD_LEVEL),
        });
        write.compress_partial_page |= force_compress_partial_page;
        let v2 = Some({
            let mut options = fwob_v2::WriterOptions::new("");
            options.codec = write.codec.codec();
            options.codec_selection = write.codec.selection();
            options.zstd_level = write.zstd_level;
            options.encoding = write.encoding.encoding();
            options.encoding_selection = write.encoding.selection();
            options.compress_partial_page = write.compress_partial_page;
            options.page_packing = write.page_packing.page_packing();
            options
        });
        (
            fwob::OperationOptions {
                reader_options: fwob::ReaderOptions { v1_key_field_index },
                v2,
                deletion_packing,
            },
            write,
        )
    }
}

pub(super) fn parse_command_tokens<'a>(
    values: &'a [String],
    allow_format: bool,
    allow_write: bool,
    allow_page_size: bool,
    allow_bench: bool,
    allow_deletion_packing: bool,
) -> Result<ParsedTokens<'a>> {
    let mut parsed = ParsedTokens::default();
    let mut seen_verify = false;
    let mut seen_compress_partial_page = false;

    for value in values {
        if allow_format {
            if let Some(format) = match_target_format(value) {
                set_once(&mut parsed.format, format, "format")?;
                continue;
            }
        }
        if allow_bench && match_bench_mode(value).is_some() {
            bail!("bench mode token '{value}' is not valid in this position");
        }
        if let Some(page_size) = parse_page_size_token(value) {
            if !allow_page_size {
                bail!("page size token '{value}' is not valid for this command");
            }
            set_once(&mut parsed.page_size, page_size?, "page size")?;
            continue;
        }
        if allow_write {
            match value.as_str() {
                "uncompressed" => {
                    set_once(&mut parsed.codec, CodecArg::Uncompressed, "codec")?;
                    continue;
                }
                "zstd" => {
                    set_once(&mut parsed.codec, CodecArg::Zstd, "codec")?;
                    continue;
                }
                "lz4" => {
                    set_once(&mut parsed.codec, CodecArg::Lz4, "codec")?;
                    continue;
                }
                "row-raw" => {
                    set_once(&mut parsed.encoding, EncodingArg::RowRaw, "encoding")?;
                    continue;
                }
                "columnar-basic" => {
                    set_once(&mut parsed.encoding, EncodingArg::ColumnarBasic, "encoding")?;
                    continue;
                }
                "columnar-delta" => {
                    set_once(&mut parsed.encoding, EncodingArg::ColumnarDelta, "encoding")?;
                    continue;
                }
                "smallest" => {
                    set_once(&mut parsed.codec, CodecArg::Smallest, "codec")?;
                    set_once(&mut parsed.encoding, EncodingArg::Smallest, "encoding")?;
                    continue;
                }
                "estimate-shrink" => {
                    set_once(
                        &mut parsed.page_packing,
                        PagePackingArg::EstimateShrink,
                        "page packing",
                    )?;
                    continue;
                }
                "tight-fit" => {
                    set_once(
                        &mut parsed.page_packing,
                        PagePackingArg::TightFit,
                        "page packing",
                    )?;
                    continue;
                }
                "verify" => {
                    set_bool_once(&mut seen_verify, "verify")?;
                    parsed.verify = true;
                    continue;
                }
                "compress-partial-page" => {
                    set_bool_once(&mut seen_compress_partial_page, "compress-partial-page")?;
                    parsed.compress_partial_page = true;
                    continue;
                }
                _ => {}
            }
        }
        if allow_deletion_packing {
            match value.as_str() {
                "local-repack" => {
                    set_once(
                        &mut parsed.deletion_packing,
                        DeletionPackingArg::LocalRepack,
                        "deletion packing",
                    )?;
                    continue;
                }
                "repack-to-end" => {
                    set_once(
                        &mut parsed.deletion_packing,
                        DeletionPackingArg::RepackToEnd,
                        "deletion packing",
                    )?;
                    continue;
                }
                _ => {}
            }
        }
        if is_any_reserved_token(value) {
            bail!("token '{value}' is not valid for this command");
        }
        parsed.paths.push(value);
    }

    Ok(parsed)
}

pub(super) fn set_once<T: Copy>(slot: &mut Option<T>, value: T, name: &str) -> Result<()> {
    if slot.is_some() {
        bail!("duplicate {name} token");
    }
    *slot = Some(value);
    Ok(())
}

fn set_bool_once(seen: &mut bool, name: &str) -> Result<()> {
    if *seen {
        bail!("duplicate {name} token");
    }
    *seen = true;
    Ok(())
}

pub(super) fn is_any_reserved_token(value: &str) -> bool {
    matches!(
        value,
        "v1" | "v2"
            | "conversion-matrix"
            | "range"
            | "random-page"
            | "scan"
            | "uncompressed"
            | "zstd"
            | "lz4"
            | "smallest"
            | "row-raw"
            | "columnar-basic"
            | "columnar-delta"
            | "estimate-shrink"
            | "tight-fit"
            | "verify"
            | "compress-partial-page"
            | "local-repack"
            | "repack-to-end"
    )
}

pub(super) fn validate_zstd_level(level: i32) -> Result<()> {
    if !(1..=22).contains(&level) {
        bail!("--zstd-level must be between 1 and 22");
    }
    Ok(())
}

pub(super) fn parse_page_size_token(value: &str) -> Option<Result<u32>> {
    const MIN_PAGE_SIZE: u64 = 1024;
    const MAX_PAGE_SIZE: u64 = 16 * 1024 * 1024;

    let (number, multiplier) = [
        ("KiB", 1024u64),
        ("MiB", 1024u64 * 1024),
        ("KB", 1000u64),
        ("MB", 1000u64 * 1000),
        ("B", 1u64),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| {
        value
            .strip_suffix(suffix)
            .filter(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
            .map(|number| (number, multiplier))
    })?;

    Some((|| {
        let number: u64 = number.parse()?;
        let size = number
            .checked_mul(multiplier)
            .context("page size is too large")?;
        if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&size) {
            bail!("page size must be between 1KiB and 16MiB");
        }
        Ok(size as u32)
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_tokens_are_case_sensitive() {
        assert!(matches!(match_target_format("v2"), Some(TargetFormat::V2)));
        assert!(match_target_format("V2").is_none());
        assert!(matches!(match_bench_mode("range"), Some(BenchMode::Range)));
        assert!(match_bench_mode("RANGE").is_none());

        let values = vec!["ZSTD".to_string(), "input.fwob".to_string()];
        let parsed = parse_command_tokens(&values, false, true, false, false, false).unwrap();
        assert_eq!(parsed.paths, ["ZSTD", "input.fwob"]);
        assert_eq!(parsed.codec, None);
    }

    #[test]
    fn write_defaults_are_shared_by_creation_and_mutation_commands() {
        let creation = V2WriteOptions::from_args(V2WriteArgs {
            zstd_level: fwob_v2::DEFAULT_ZSTD_LEVEL,
        });
        let parsed = ParsedTokens::default();
        let (mutation, resolved) =
            parsed.operation_options(0, None, fwob::DeletionPacking::LocalRepack, false);
        let mutation = mutation.v2.expect("mutation defaults are explicit");

        assert_eq!(resolved.codec, creation.codec);
        assert_eq!(resolved.encoding, creation.encoding);
        assert_eq!(resolved.zstd_level, creation.zstd_level);
        assert_eq!(resolved.page_packing, creation.page_packing);
        assert_eq!(mutation.codec, fwob_v2::DEFAULT_CODEC);
        assert_eq!(mutation.encoding, fwob_v2::DEFAULT_ENCODING);
        assert_eq!(mutation.zstd_level, fwob_v2::DEFAULT_ZSTD_LEVEL);
        assert_eq!(mutation.page_packing, fwob_v2::DEFAULT_PAGE_PACKING);
    }

    #[test]
    fn convert_defaults_to_v2() {
        assert!(matches!(
            parse_convert_target(
                &["input.fwob".to_owned(), "output.fwob".to_owned()],
                V2WriteArgs {
                    zstd_level: fwob_v2::DEFAULT_ZSTD_LEVEL,
                },
            )
            .unwrap()
            .0,
            TargetFormat::V2
        ));
    }
}
