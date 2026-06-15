use fwob::FormatVersion;

pub fn for_each_format(mut test: impl FnMut(FormatVersion)) {
    for version in [FormatVersion::V1, FormatVersion::V2] {
        test(version);
    }
}
