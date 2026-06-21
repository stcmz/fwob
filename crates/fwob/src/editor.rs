use std::{
    fs::{File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};

use fwob_core::{Key, Schema};
use tempfile::NamedTempFile;

use crate::{Error, FormatVersion, OperationOptions, Reader, ReaderOptions, Result};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeletionPacking {
    #[default]
    LocalRepack,
    RepackToEnd,
}

pub struct Editor {
    path: PathBuf,
    format_version: FormatVersion,
    schema: Schema,
    title: String,
    string_table: Vec<String>,
    frame_count: u64,
    v1_key_field_index: usize,
    operation_options: OperationOptions,
}

impl Editor {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_operation_options(path, OperationOptions::default())
    }

    pub fn open_with_options(path: impl AsRef<Path>, options: ReaderOptions) -> Result<Self> {
        Self::open_with_operation_options(
            path,
            OperationOptions {
                reader_options: options,
                ..Default::default()
            },
        )
    }

    pub fn open_with_operation_options(
        path: impl AsRef<Path>,
        options: OperationOptions,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let reader = Reader::open_with_options(&path, options.reader_options)?;
        let format_version = reader.format_version();
        let schema = reader.schema().clone();
        let title = reader.title().to_owned();
        let string_table = reader.string_table().to_vec();
        let frame_count = reader.frame_count();
        Ok(Self {
            path,
            format_version,
            schema,
            title,
            string_table,
            frame_count,
            v1_key_field_index: options.reader_options.v1_key_field_index,
            operation_options: options,
        })
    }

    fn rewrite_without(&mut self, deleted: Range<u64>) -> Result<u64> {
        self.delete_ranges(std::slice::from_ref(&deleted))
    }

    fn delete_validated_ranges(&mut self, ranges: &[Range<u64>]) -> Result<u64> {
        let removed = validate_ranges(ranges, self.frame_count)?;
        if removed == 0 {
            return Ok(0);
        }
        match self.format_version {
            FormatVersion::V1 => {
                fwob_v1::delete_frame_ranges(&self.path, ranges)?;
                self.frame_count -= removed;
                Ok(removed)
            }
            FormatVersion::V2 => {
                fwob_v2_delete_ranges(&self.path, ranges, removed, &self.operation_options)?;
                self.frame_count -= removed;
                Ok(removed)
            }
        }
    }

    pub fn update_metadata(
        &mut self,
        frame_type: Option<&str>,
        title: Option<&str>,
        string_table: Option<&[String]>,
    ) -> Result<()> {
        let new_frame_type = frame_type.unwrap_or(&self.schema.frame_type).to_owned();
        let new_title = title.unwrap_or(&self.title).to_owned();
        let new_string_table = string_table.unwrap_or(&self.string_table).to_vec();
        if new_frame_type == self.schema.frame_type
            && new_title == self.title
            && new_string_table == self.string_table
        {
            return Ok(());
        }
        match self.format_version {
            FormatVersion::V1 => {
                fwob_v1::update_metadata(&self.path, frame_type, title, string_table)?;
            }
            FormatVersion::V2 => {
                fwob_v2::update_metadata(&self.path, frame_type, title, string_table)?;
            }
        }
        self.schema.frame_type = new_frame_type;
        self.title = new_title;
        self.string_table = new_string_table;
        Ok(())
    }

    pub fn format_version(&self) -> FormatVersion {
        self.format_version
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn string_table(&self) -> &[String] {
        &self.string_table
    }

    pub fn delete_frame(&mut self, index: u64) -> Result<bool> {
        if index >= self.frame_count {
            return Ok(false);
        }
        Ok(self.rewrite_without(index..index + 1)? == 1)
    }

    pub fn delete_frames(&mut self, range: Range<u64>) -> Result<u64> {
        self.delete_ranges(std::slice::from_ref(&range))
    }

    pub fn delete_indices(&mut self, indices: &[u64]) -> Result<u64> {
        if indices.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(fwob_core::FwobError::InvalidSchema(
                "frame indices must be strictly increasing".into(),
            )
            .into());
        }
        let mut ranges = Vec::with_capacity(indices.len());
        for &index in indices {
            let end =
                index
                    .checked_add(1)
                    .ok_or_else(|| fwob_core::FwobError::InvalidFrameRange {
                        start: index,
                        end: index,
                        frame_count: self.frame_count,
                    })?;
            ranges.push(index..end);
        }
        self.delete_ranges(&ranges)
    }

    pub fn delete_ranges(&mut self, ranges: &[Range<u64>]) -> Result<u64> {
        self.delete_validated_ranges(ranges)
    }

    pub fn delete_key(&mut self, key: Key) -> Result<u64> {
        let mut reader = Reader::open_with_options(
            &self.path,
            ReaderOptions {
                v1_key_field_index: self.v1_key_field_index,
            },
        )?;
        let range = reader.equal_range(key)?;
        drop(reader);
        self.rewrite_without(range)
    }

    pub fn delete_keys(&mut self, keys: &[Key]) -> Result<u64> {
        if keys.windows(2).any(|pair| pair[0] > pair[1]) {
            return Err(fwob_core::FwobError::UnsortedKeys.into());
        }
        let mut reader = Reader::open_with_options(
            &self.path,
            ReaderOptions {
                v1_key_field_index: self.v1_key_field_index,
            },
        )?;
        let mut ranges = Vec::with_capacity(keys.len());
        let mut minimum = 0;
        for key in keys {
            let mut range = reader.equal_range(*key)?;
            range.start = range.start.max(minimum);
            if range.start < range.end {
                minimum = range.end;
                ranges.push(range);
            }
        }
        drop(reader);
        self.delete_ranges(&ranges)
    }

    pub fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> Result<u64> {
        if range.start() > range.end() {
            return Ok(0);
        }
        let mut reader = Reader::open_with_options(
            &self.path,
            ReaderOptions {
                v1_key_field_index: self.v1_key_field_index,
            },
        )?;
        let start = reader.lower_bound(*range.start())?;
        let end = reader.upper_bound(*range.end())?;
        drop(reader);
        self.rewrite_without(start..end)
    }

    pub fn delete_before(&mut self, last_key: Key) -> Result<u64> {
        let mut reader = Reader::open_with_options(
            &self.path,
            ReaderOptions {
                v1_key_field_index: self.v1_key_field_index,
            },
        )?;
        let end = reader.upper_bound(last_key)?;
        drop(reader);
        self.rewrite_without(0..end)
    }

    pub fn delete_after(&mut self, first_key: Key) -> Result<u64> {
        let mut reader = Reader::open_with_options(
            &self.path,
            ReaderOptions {
                v1_key_field_index: self.v1_key_field_index,
            },
        )?;
        let start = reader.lower_bound(first_key)?;
        drop(reader);
        self.rewrite_without(start..self.frame_count)
    }

    pub fn delete_all_frames(&mut self) -> Result<u64> {
        self.rewrite_without(0..self.frame_count)
    }

    pub fn set_title(&mut self, title: &str) -> Result<()> {
        self.update_metadata(None, Some(title), None)
    }

    pub fn set_frame_type(&mut self, frame_type: &str) -> Result<()> {
        self.update_metadata(Some(frame_type), None, None)
    }

    pub fn append_string(&mut self, value: &str) -> Result<u32> {
        let index = u32::try_from(self.string_table.len()).map_err(|_| {
            Error::Core(fwob_core::FwobError::InvalidSchema(
                "string table exceeds u32 entries".into(),
            ))
        })?;
        let mut strings = self.string_table.clone();
        strings.push(value.to_owned());
        self.update_metadata(None, None, Some(&strings))?;
        Ok(index)
    }

    pub fn replace_string_table(&mut self, strings: &[String]) -> Result<()> {
        self.update_metadata(None, None, Some(strings))
    }

    pub fn clear_string_table(&mut self) -> Result<()> {
        self.replace_string_table(&[])
    }
}

fn fwob_v2_delete_ranges(
    path: &Path,
    ranges: &[Range<u64>],
    removed: u64,
    operation_options: &OperationOptions,
) -> Result<()> {
    let mut reader = fwob_v2::Reader::open(path)?;
    let header = reader.header().clone();
    let first_deleted = ranges.first().expect("validated non-empty ranges").start;
    let last_deleted = ranges.last().expect("validated non-empty ranges").end - 1;
    let start_page = reader
        .find_page_for_index(first_deleted)?
        .expect("validated deletion start");
    let last_affected_page = reader
        .find_page_for_index(last_deleted)?
        .expect("validated deletion end");
    let mut end_page = match operation_options.deletion_packing {
        DeletionPacking::LocalRepack => last_affected_page,
        DeletionPacking::RepackToEnd => header.page_count - 1,
    };
    let start_header = reader.read_page_header(start_page)?;
    let first_frame_index = start_header.first_frame_index;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = NamedTempFile::new_in(parent)?;
    let temporary_path = temporary.into_temp_path();
    let mut options = operation_options.v2.clone().unwrap_or_else(|| {
        let mut options = fwob_v2::WriterOptions::new("");
        options.codec = start_header.codec;
        options.codec_selection = fwob_v2::CodecSelection::Fixed(start_header.codec);
        options.encoding = start_header.encoding;
        options.encoding_selection = fwob_v2::EncodingSelection::Fixed(start_header.encoding);
        options
    });
    options.title = header.title.clone();
    options.page_size = header.page_size;
    options.string_table = header.string_table.clone();
    if matches!(
        operation_options.deletion_packing,
        DeletionPacking::LocalRepack
    ) {
        options.compress_partial_page = true;
    }
    let frame_len = header.schema.frame_len as usize;
    let replacement_header = loop {
        let mut writer =
            fwob_v2::Writer::create(&temporary_path, header.schema.clone(), options.clone())?;
        let mut range_index = 0usize;
        for page_index in start_page..=end_page {
            let page = reader.read_page_header(page_index)?;
            let raw = reader.read_page_raw_frames(page_index)?;
            let mut retained = Vec::with_capacity(raw.len());
            for local_index in 0..u64::from(page.frame_count) {
                let global_index = page.first_frame_index + local_index;
                while range_index < ranges.len() && ranges[range_index].end <= global_index {
                    range_index += 1;
                }
                let deleted = range_index < ranges.len()
                    && ranges[range_index].start <= global_index
                    && global_index < ranges[range_index].end;
                if !deleted {
                    let offset = local_index as usize * frame_len;
                    retained.extend_from_slice(&raw[offset..offset + frame_len]);
                }
            }
            writer.append_presorted_raw_frames(&retained)?;
        }
        writer.finish()?;
        let replacement_header = fwob_v2::Reader::open(&temporary_path)?.header().clone();
        let consumed_pages = end_page - start_page + 1;
        if replacement_header.page_count <= consumed_pages || end_page + 1 == header.page_count {
            break replacement_header;
        }
        end_page += 1;
    };
    drop(reader);

    let mut source = OpenOptions::new().read(true).write(true).open(path)?;
    let mut replacement = File::open(&temporary_path)?;
    let page_size = header.page_size as usize;
    let mut page_bytes = vec![0u8; page_size];

    for replacement_index in 0..replacement_header.page_count {
        replacement.seek(SeekFrom::Start(
            fwob_v2::FILE_HEADER_LEN + replacement_index * u64::from(header.page_size),
        ))?;
        replacement.read_exact(&mut page_bytes)?;
        let local_first_frame_index =
            read_page_header(&page_bytes, replacement_index)?.first_frame_index;
        rewrite_page_index(
            &mut page_bytes,
            replacement_index,
            first_frame_index + local_first_frame_index,
        )?;
        source.seek(SeekFrom::Start(
            header.page_offset(start_page + replacement_index),
        ))?;
        source.write_all(&page_bytes)?;
    }

    let tail_source_page = end_page + 1;
    let tail_destination_page = start_page + replacement_header.page_count;
    for source_page in tail_source_page..header.page_count {
        source.seek(SeekFrom::Start(header.page_offset(source_page)))?;
        source.read_exact(&mut page_bytes)?;
        let old_index = read_page_header(&page_bytes, source_page)?.first_frame_index;
        rewrite_page_index(&mut page_bytes, source_page, old_index - removed)?;
        let destination_page = tail_destination_page + (source_page - tail_source_page);
        source.seek(SeekFrom::Start(header.page_offset(destination_page)))?;
        source.write_all(&page_bytes)?;
    }

    let consumed_pages = end_page - start_page + 1;
    let new_page_count = header.page_count - consumed_pages + replacement_header.page_count;
    let new_frame_count = header.frame_count - removed;
    source.set_len(fwob_v2::FILE_HEADER_LEN + new_page_count * u64::from(header.page_size))?;
    fwob_v2::update_counts(&mut source, new_page_count, new_frame_count)?;
    source.flush()?;
    Ok(())
}

fn read_page_header(bytes: &[u8], page_index: u64) -> Result<fwob_v2::PageHeader> {
    Ok(fwob_v2::PageHeader::read(
        &mut Cursor::new(&bytes[..fwob_v2::PAGE_HEADER_LEN]),
        page_index,
    )?)
}

fn rewrite_page_index(bytes: &mut [u8], page_index: u64, first_frame_index: u64) -> Result<()> {
    let mut page = read_page_header(bytes, page_index)?;
    page.set_first_frame_index(first_frame_index);
    page.write(&mut Cursor::new(&mut bytes[..fwob_v2::PAGE_HEADER_LEN]))?;
    Ok(())
}

impl fwob_core::FileInfo for Editor {
    fn format_version(&self) -> FormatVersion {
        self.format_version
    }

    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn frame_count(&self) -> u64 {
        self.frame_count
    }

    fn string_table(&self) -> &[String] {
        &self.string_table
    }
}

impl fwob_core::Editor for Editor {
    fn delete_frame(&mut self, index: u64) -> fwob_core::Result<bool> {
        Editor::delete_frame(self, index).map_err(fwob_core::FwobError::backend)
    }

    fn delete_frames(&mut self, range: Range<u64>) -> fwob_core::Result<u64> {
        Editor::delete_frames(self, range).map_err(fwob_core::FwobError::backend)
    }

    fn delete_indices(&mut self, indices: &[u64]) -> fwob_core::Result<u64> {
        Editor::delete_indices(self, indices).map_err(fwob_core::FwobError::backend)
    }

    fn delete_ranges(&mut self, ranges: &[Range<u64>]) -> fwob_core::Result<u64> {
        Editor::delete_ranges(self, ranges).map_err(fwob_core::FwobError::backend)
    }

    fn delete_key(&mut self, key: Key) -> fwob_core::Result<u64> {
        Editor::delete_key(self, key).map_err(fwob_core::FwobError::backend)
    }

    fn delete_keys(&mut self, keys: &[Key]) -> fwob_core::Result<u64> {
        Editor::delete_keys(self, keys).map_err(fwob_core::FwobError::backend)
    }

    fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> fwob_core::Result<u64> {
        Editor::delete_key_range(self, range).map_err(fwob_core::FwobError::backend)
    }

    fn delete_before(&mut self, last_key: Key) -> fwob_core::Result<u64> {
        Editor::delete_before(self, last_key).map_err(fwob_core::FwobError::backend)
    }

    fn delete_after(&mut self, first_key: Key) -> fwob_core::Result<u64> {
        Editor::delete_after(self, first_key).map_err(fwob_core::FwobError::backend)
    }

    fn delete_all_frames(&mut self) -> fwob_core::Result<u64> {
        Editor::delete_all_frames(self).map_err(fwob_core::FwobError::backend)
    }

    fn set_title(&mut self, title: &str) -> fwob_core::Result<()> {
        Editor::set_title(self, title).map_err(fwob_core::FwobError::backend)
    }

    fn append_string(&mut self, value: &str) -> fwob_core::Result<u32> {
        Editor::append_string(self, value).map_err(fwob_core::FwobError::backend)
    }

    fn replace_string_table(&mut self, strings: &[String]) -> fwob_core::Result<()> {
        Editor::replace_string_table(self, strings).map_err(fwob_core::FwobError::backend)
    }
}

fn validate_ranges(ranges: &[Range<u64>], frame_count: u64) -> Result<u64> {
    let mut previous_end = 0;
    let mut removed = 0u64;
    for range in ranges {
        if range.start < previous_end || range.start > range.end || range.end > frame_count {
            return Err(fwob_core::FwobError::InvalidFrameRange {
                start: range.start,
                end: range.end,
                frame_count,
            }
            .into());
        }
        previous_end = range.end;
        removed += range.end - range.start;
    }
    Ok(removed)
}
