use syn::{Attribute, Error, Ident, LitStr};

#[derive(Debug, Default)]
pub(crate) struct FieldAttributes {
    pub(crate) is_key: bool,
    pub(crate) ignored: bool,
    pub(crate) string_index: bool,
    pub(crate) timestamp: Option<String>,
}

impl FieldAttributes {
    pub(crate) fn parse(attributes: &[Attribute], ident: &Ident) -> syn::Result<Self> {
        let mut parsed = Self::default();
        for attribute in attributes {
            if !attribute.path().is_ident("fwob") {
                continue;
            }
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("key") {
                    parsed.is_key = true;
                    Ok(())
                } else if meta.path.is_ident("ignore") {
                    parsed.ignored = true;
                    Ok(())
                } else if meta.path.is_ident("string_index") {
                    parsed.string_index = true;
                    Ok(())
                } else if meta.path.is_ident("timestamp") {
                    parsed.timestamp = Some(meta.value()?.parse::<LitStr>()?.value());
                    Ok(())
                } else {
                    Err(meta.error("supported attributes: key, ignore, string_index, timestamp"))
                }
            })?;
        }
        if parsed.ignored && parsed.is_key {
            return Err(Error::new_spanned(ident, "the key field cannot be ignored"));
        }
        if parsed.ignored && parsed.timestamp.is_some() {
            return Err(Error::new_spanned(
                ident,
                "an ignored field cannot be a timestamp",
            ));
        }
        Ok(parsed)
    }
}
