use super::*;

#[derive(Default)]
pub(super) struct V2Metadata {
    pub(super) physical_bytes: u64,
    pub(super) expected_physical_bytes: u64,
    pub(super) payload_capacity_per_page: u64,
    pub(super) payload_capacity_total: u64,
    pub(super) compressed_total: u64,
    pub(super) uncompressed_total: u64,
    pub(super) padding_bytes: u64,
    pub(super) min_frames: u32,
    pub(super) max_frames: u32,
    pub(super) first_key: Option<fwob_core::Key>,
    pub(super) last_key: Option<fwob_core::Key>,
    pub(super) codec_none_pages: u64,
    pub(super) codec_zstd_pages: u64,
    pub(super) codec_lz4_pages: u64,
    pub(super) compressed_page_frames: u64,
    pub(super) encoding_row_raw_v1_pages: u64,
    pub(super) encoding_columnar_basic_v1_pages: u64,
    pub(super) encoding_columnar_delta_v1_pages: u64,
}

pub(super) fn collect_v2_metadata(
    path: &Path,
    reader: &mut fwob_v2::Reader<std::fs::File>,
) -> Result<V2Metadata> {
    let page_count = reader.header().page_count;
    let page_size = u64::from(reader.header().page_size);
    let payload_capacity_per_page = page_size.saturating_sub(fwob_v2::PAGE_HEADER_LEN as u64);
    let payload_capacity_total = page_count * payload_capacity_per_page;

    let mut compressed_total = 0u64;
    let mut uncompressed_total = 0u64;
    let mut min_frames = u32::MAX;
    let mut max_frames = 0u32;
    let mut first_key = None;
    let mut last_key = None;
    let mut codec_none_pages = 0u64;
    let mut codec_zstd_pages = 0u64;
    let mut codec_lz4_pages = 0u64;
    let mut compressed_page_frames = 0u64;
    let mut encoding_row_raw_v1_pages = 0u64;
    let mut encoding_columnar_basic_v1_pages = 0u64;
    let mut encoding_columnar_delta_v1_pages = 0u64;

    for page_index in 0..page_count {
        let page = reader.read_page_header(page_index)?;
        compressed_total += u64::from(page.compressed_len);
        uncompressed_total += u64::from(page.uncompressed_len);
        min_frames = min_frames.min(page.frame_count);
        max_frames = max_frames.max(page.frame_count);
        if first_key.is_none() {
            first_key = Some(page.first_key);
        }
        last_key = Some(page.last_key);

        match page.codec {
            fwob_v2::Codec::None => codec_none_pages += 1,
            fwob_v2::Codec::Zstd => {
                codec_zstd_pages += 1;
                compressed_page_frames += u64::from(page.frame_count);
            }
            fwob_v2::Codec::Lz4 => {
                codec_lz4_pages += 1;
                compressed_page_frames += u64::from(page.frame_count);
            }
        }
        match page.encoding {
            fwob_v2::Encoding::RowRawV1 => encoding_row_raw_v1_pages += 1,
            fwob_v2::Encoding::ColumnarBasicV1 => encoding_columnar_basic_v1_pages += 1,
            fwob_v2::Encoding::ColumnarDeltaV1 => encoding_columnar_delta_v1_pages += 1,
        }
    }

    let physical_bytes = std::fs::metadata(path)?.len();
    let expected_physical_bytes = fwob_v2::FILE_HEADER_LEN + page_count * page_size;
    let padding_bytes = payload_capacity_total.saturating_sub(compressed_total);

    Ok(V2Metadata {
        physical_bytes,
        expected_physical_bytes,
        payload_capacity_per_page,
        payload_capacity_total,
        compressed_total,
        uncompressed_total,
        padding_bytes,
        min_frames,
        max_frames,
        first_key,
        last_key,
        codec_none_pages,
        codec_zstd_pages,
        codec_lz4_pages,
        compressed_page_frames,
        encoding_row_raw_v1_pages,
        encoding_columnar_basic_v1_pages,
        encoding_columnar_delta_v1_pages,
    })
}
