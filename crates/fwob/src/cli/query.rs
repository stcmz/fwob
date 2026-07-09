use std::io::Write;

use anyhow::{bail, Context, Result};

use super::inspect::{format_frame_preview_rows, preview_indices, PreviewIndex, PreviewRow};
use super::{color_enabled, TomlWriter};
use super::{resolve_selectors, CatArgs, FindArgs};

pub(super) fn dump_frames(args: CatArgs) -> Result<()> {
    let mut format = None;
    let mut selector_values = Vec::new();
    for value in &args.target {
        if let Some(parsed) = fwob::formatting::FrameFormat::parse(value) {
            if format.replace(parsed).is_some() {
                bail!("dump accepts at most one output format token");
            }
        } else {
            selector_values.push(value);
        }
    }

    let reader_options = fwob::ReaderOptions {
        v1_key_field_index: args.key_field_index,
    };
    let mut reader = fwob::Reader::open_with_options(&args.path, reader_options)?;
    let schema = reader.schema().clone();
    let string_table = reader.string_table().to_vec();
    let resolved = resolve_selectors(&mut reader, selector_values.into_iter().map(String::as_str))?;
    let selection = resolved.selection;
    let mut formatter = fwob::formatting::FrameFormatter::new(
        &schema,
        &string_table,
        format.unwrap_or(fwob::formatting::FrameFormat::Table),
    );
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    formatter.write_header(&mut output)?;
    for range in selection.ranges() {
        for frame in reader.frames(range.clone())? {
            formatter.write_frame(&mut output, frame?.bytes())?;
        }
    }
    output.flush()?;
    Ok(())
}

pub(super) fn find_frames(args: FindArgs) -> Result<()> {
    let mut w = TomlWriter::new(std::io::stdout(), color_enabled());
    let reader_options = fwob::ReaderOptions {
        v1_key_field_index: args.key_field_index,
    };
    let mut reader = fwob::Reader::open_with_options(&args.path, reader_options)?;
    let schema = reader.schema().clone();
    let resolved = resolve_selectors(&mut reader, args.selectors.iter().map(String::as_str))?;
    let selection = resolved.selection;
    let rows = selection_preview_rows(&mut reader, &selection)?;

    w.section("find")?;
    w.kv_str("path", &args.path.display().to_string())?;
    w.kv_num("selector_count", resolved.selector_count)?;
    w.kv_num("range_count", selection.ranges().len())?;
    if let Some(start) = selection.first_index() {
        w.kv_num("start_index", start)?;
        w.kv_num("end_index", selection.end_index().unwrap())?;
    }
    w.kv_num("frame_count", selection.frame_count())?;
    if !rows.is_empty() {
        println!();
        w.section("frames")?;
        w.kv_multiline("preview", &format_frame_preview_rows(&schema, &rows))?;
    }
    Ok(())
}

fn selection_preview_rows(
    reader: &mut fwob::Reader,
    selection: &fwob::FrameSelection,
) -> Result<Vec<PreviewRow>> {
    let count = selection.frame_count();
    let positions = preview_indices(count);
    let mut rows = Vec::with_capacity(positions.len());
    for position in positions {
        match position {
            PreviewIndex::Ellipsis => rows.push(PreviewRow::Ellipsis),
            PreviewIndex::Frame(position) => {
                let mut remaining = position;
                let mut global_index = None;
                for range in selection.ranges() {
                    let len = range.end - range.start;
                    if remaining < len {
                        global_index = Some(range.start + remaining);
                        break;
                    }
                    remaining -= len;
                }
                let global_index =
                    global_index.context("selected preview index is out of range")?;
                let frame = reader
                    .read_frame(global_index)?
                    .context("matched frame index is out of range")?;
                rows.push(PreviewRow::Frame(global_index, frame.bytes().to_vec()));
            }
        }
    }
    Ok(rows)
}
