use anyhow::Result;

pub(super) struct ResolvedSelectors {
    pub(super) selector_count: usize,
    pub(super) selection: fwob::FrameSelection,
}

pub(super) fn resolve_selectors<'a>(
    reader: &mut fwob::Reader,
    values: impl IntoIterator<Item = &'a str>,
) -> Result<ResolvedSelectors> {
    let key_type = fwob_core::KeyType::from_field(reader.schema().key_field())?;
    let selectors = values
        .into_iter()
        .map(|value| fwob::KeySelector::parse(value, key_type))
        .collect::<fwob::Result<Vec<_>>>()?;
    let selection = fwob::FrameSelection::resolve(reader, &selectors)?;
    Ok(ResolvedSelectors {
        selector_count: selectors.len(),
        selection,
    })
}
