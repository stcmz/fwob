use std::{
    ops::{Range, RangeInclusive},
    path::{Path, PathBuf},
};

use fwob_core::{Key, Schema};
use tempfile::NamedTempFile;

use crate::{
    verify_file, AnyReader, Error, FormatVersion, FwobEditor, FwobFile, Result, VerificationOptions,
};

const COPY_BUFFER_BYTES: usize = 4 * 1024 * 1024;

pub struct AnyEditor {
    path: PathBuf,
    format_version: FormatVersion,
    schema: Schema,
    title: String,
    string_table: Vec<String>,
    frame_count: u64,
    v1_key_field_index: usize,
}

impl AnyEditor {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_v1_key(path, 0)
    }

    pub fn open_with_v1_key(path: impl AsRef<Path>, v1_key_field_index: usize) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let reader = AnyReader::open_with_v1_key(&path, v1_key_field_index)?;
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
            v1_key_field_index,
        })
    }

    fn rewrite(
        &mut self,
        deleted: Range<u64>,
        title: String,
        string_table: Vec<String>,
    ) -> Result<u64> {
        if deleted.start > deleted.end || deleted.end > self.frame_count {
            return Err(Error::InvalidFrameRange {
                start: deleted.start,
                end: deleted.end,
                frame_count: self.frame_count,
            });
        }
        let removed = deleted.end - deleted.start;
        if removed == 0 && title == self.title && string_table == self.string_table {
            return Ok(0);
        }

        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let temporary = NamedTempFile::new_in(parent)?;
        let temporary_path = temporary.into_temp_path();
        let mut source = AnyReader::open_with_v1_key(&self.path, self.v1_key_field_index)?;
        let mut destination =
            source.create_compatible_writer(&temporary_path, &title, &string_table)?;

        copy_range(
            &mut source,
            &mut destination,
            0..deleted.start,
            self.schema.frame_len as usize,
        )?;
        copy_range(
            &mut source,
            &mut destination,
            deleted.end..self.frame_count,
            self.schema.frame_len as usize,
        )?;
        destination.finish()?;
        drop(source);

        verify_file(
            &temporary_path,
            VerificationOptions {
                v1_key_field_index: self.v1_key_field_index,
            },
        )?;
        temporary_path
            .persist(&self.path)
            .map_err(|error| Error::Io(error.error))?;
        self.frame_count -= removed;
        self.title = title;
        self.string_table = string_table;
        Ok(removed)
    }

    fn rewrite_without(&mut self, deleted: Range<u64>) -> Result<u64> {
        self.rewrite(deleted, self.title.clone(), self.string_table.clone())
    }

    pub fn update_metadata(
        &mut self,
        title: Option<&str>,
        string_table: Option<&[String]>,
    ) -> Result<()> {
        self.rewrite(
            0..0,
            title.unwrap_or(&self.title).to_owned(),
            string_table.unwrap_or(&self.string_table).to_vec(),
        )?;
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
        FwobEditor::delete_frame(self, index)
    }

    pub fn delete_frames(&mut self, range: Range<u64>) -> Result<u64> {
        FwobEditor::delete_frames(self, range)
    }

    pub fn delete_key(&mut self, key: Key) -> Result<u64> {
        FwobEditor::delete_key(self, key)
    }

    pub fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> Result<u64> {
        FwobEditor::delete_key_range(self, range)
    }

    pub fn delete_all_frames(&mut self) -> Result<u64> {
        FwobEditor::delete_all_frames(self)
    }

    pub fn set_title(&mut self, title: &str) -> Result<()> {
        FwobEditor::set_title(self, title)
    }

    pub fn append_string(&mut self, value: &str) -> Result<u32> {
        FwobEditor::append_string(self, value)
    }

    pub fn replace_string_table(&mut self, strings: &[String]) -> Result<()> {
        FwobEditor::replace_string_table(self, strings)
    }

    pub fn clear_string_table(&mut self) -> Result<()> {
        FwobEditor::clear_string_table(self)
    }
}

impl FwobFile for AnyEditor {
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

impl FwobEditor for AnyEditor {
    fn delete_frame(&mut self, index: u64) -> Result<bool> {
        if index >= self.frame_count {
            return Ok(false);
        }
        Ok(self.rewrite_without(index..index + 1)? == 1)
    }

    fn delete_frames(&mut self, range: Range<u64>) -> Result<u64> {
        self.rewrite_without(range)
    }

    fn delete_key(&mut self, key: Key) -> Result<u64> {
        let mut reader = AnyReader::open_with_v1_key(&self.path, self.v1_key_field_index)?;
        let range = reader.equal_range(key)?;
        drop(reader);
        self.rewrite_without(range)
    }

    fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> Result<u64> {
        if range.start() > range.end() {
            return Ok(0);
        }
        let mut reader = AnyReader::open_with_v1_key(&self.path, self.v1_key_field_index)?;
        let start = reader.lower_bound(*range.start())?;
        let end = reader.upper_bound(*range.end())?;
        drop(reader);
        self.rewrite_without(start..end)
    }

    fn delete_all_frames(&mut self) -> Result<u64> {
        self.rewrite_without(0..self.frame_count)
    }

    fn set_title(&mut self, title: &str) -> Result<()> {
        self.update_metadata(Some(title), None)
    }

    fn append_string(&mut self, value: &str) -> Result<u32> {
        let index = u32::try_from(self.string_table.len()).map_err(|_| {
            Error::Core(fwob_core::FwobError::InvalidSchema(
                "string table exceeds u32 entries".into(),
            ))
        })?;
        let mut strings = self.string_table.clone();
        strings.push(value.to_owned());
        self.rewrite(0..0, self.title.clone(), strings)?;
        Ok(index)
    }

    fn replace_string_table(&mut self, strings: &[String]) -> Result<()> {
        self.update_metadata(None, Some(strings))
    }
}

impl fwob_core::FileInfo for AnyEditor {
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

impl fwob_core::Editor for AnyEditor {
    fn delete_frame(&mut self, index: u64) -> fwob_core::Result<bool> {
        FwobEditor::delete_frame(self, index).map_err(fwob_core::FwobError::backend)
    }

    fn delete_frames(&mut self, range: Range<u64>) -> fwob_core::Result<u64> {
        FwobEditor::delete_frames(self, range).map_err(fwob_core::FwobError::backend)
    }

    fn delete_key(&mut self, key: Key) -> fwob_core::Result<u64> {
        FwobEditor::delete_key(self, key).map_err(fwob_core::FwobError::backend)
    }

    fn delete_key_range(&mut self, range: RangeInclusive<Key>) -> fwob_core::Result<u64> {
        FwobEditor::delete_key_range(self, range).map_err(fwob_core::FwobError::backend)
    }

    fn delete_all_frames(&mut self) -> fwob_core::Result<u64> {
        FwobEditor::delete_all_frames(self).map_err(fwob_core::FwobError::backend)
    }

    fn set_title(&mut self, title: &str) -> fwob_core::Result<()> {
        FwobEditor::set_title(self, title).map_err(fwob_core::FwobError::backend)
    }

    fn append_string(&mut self, value: &str) -> fwob_core::Result<u32> {
        FwobEditor::append_string(self, value).map_err(fwob_core::FwobError::backend)
    }

    fn replace_string_table(&mut self, strings: &[String]) -> fwob_core::Result<()> {
        FwobEditor::replace_string_table(self, strings).map_err(fwob_core::FwobError::backend)
    }
}

fn copy_range(
    source: &mut AnyReader,
    destination: &mut fwob_core::Writer,
    range: Range<u64>,
    frame_len: usize,
) -> Result<()> {
    let frames_per_buffer = (COPY_BUFFER_BYTES / frame_len).max(1);
    let mut next = range.start;
    let mut bytes = Vec::with_capacity(frames_per_buffer * frame_len);
    while next < range.end {
        bytes.clear();
        let chunk_end = range.end.min(next + frames_per_buffer as u64);
        for frame in source.frames(next..chunk_end)? {
            bytes.extend_from_slice(frame?.bytes());
        }
        destination.append_presorted_frames(&bytes)?;
        next = chunk_end;
    }
    Ok(())
}
