use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse_macro_input, Data, DeriveInput, Error, Fields, GenericArgument, LitInt, PathArguments,
    Type,
};

#[proc_macro_derive(FwobFrame, attributes(fwob))]
pub fn derive_fwob_frame(input: TokenStream) -> TokenStream {
    derive(parse_macro_input!(input as DeriveInput))
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn derive(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = input.ident;
    let Data::Struct(data) = input.data else {
        return Err(Error::new_spanned(name, "FwobFrame requires a struct"));
    };
    let Fields::Named(fields) = data.fields else {
        return Err(Error::new_spanned(
            name,
            "FwobFrame requires named struct fields",
        ));
    };

    let mut schema_fields = Vec::new();
    let mut encoders = Vec::new();
    let mut decoders = Vec::new();
    let mut initializers = Vec::new();
    let mut key_field = None;
    let mut stored_index = 0usize;
    let mut frame_len = 0usize;

    for field in fields.named {
        let ident = field.ident.expect("named field");
        let mut is_key = false;
        let mut ignored = false;
        let mut string_index = false;
        for attribute in &field.attrs {
            if !attribute.path().is_ident("fwob") {
                continue;
            }
            attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("key") {
                    is_key = true;
                    Ok(())
                } else if meta.path.is_ident("ignore") {
                    ignored = true;
                    Ok(())
                } else if meta.path.is_ident("string_index") {
                    string_index = true;
                    Ok(())
                } else {
                    Err(meta.error("supported attributes: key, ignore, string_index"))
                }
            })?;
        }

        if ignored {
            if is_key {
                return Err(Error::new_spanned(ident, "the key field cannot be ignored"));
            }
            initializers.push(quote!(#ident: ::core::default::Default::default()));
            continue;
        }

        let info = field_info(&field.ty, string_index)?;
        let field_name = ident.to_string();
        let field_type = &info.field_type;
        let length = info.length;
        frame_len += usize::from(length);
        let encode = info.encode(&ident);
        let local = format_ident!("__fwob_{}", ident);
        let decode = info.decode(&local);

        schema_fields.push(quote! {
            ::fwob_core::Field::new(
                #field_name,
                #field_type,
                #length,
                __fwob_offset,
            )
        });
        encoders.push(encode);
        decoders.push(decode);
        initializers.push(quote!(#ident: #local));

        if is_key {
            if key_field.is_some() {
                return Err(Error::new_spanned(ident, "only one key field is allowed"));
            }
            key_field = Some((stored_index, ident.clone(), field.ty.clone()));
        }
        stored_index += 1;
    }

    let Some((key_index, key_ident, key_type)) = key_field else {
        return Err(Error::new_spanned(name, "one field must use #[fwob(key)]"));
    };
    let frame_name = name.to_string();

    Ok(quote! {
        impl ::fwob_core::FwobFrame for #name {
            type Key = #key_type;

            fn schema() -> ::fwob_core::Schema {
                let mut __fwob_offset = 0u32;
                let mut __fwob_fields = ::std::vec::Vec::new();
                #(
                    let __fwob_field = #schema_fields;
                    __fwob_offset += u32::from(__fwob_field.length);
                    __fwob_fields.push(__fwob_field);
                )*
                ::fwob_core::Schema::new(#frame_name, __fwob_fields, #key_index)
                    .expect("derived FWOB schema is valid")
            }

            fn key(&self) -> Self::Key {
                self.#key_ident
            }

            fn encode(&self, __fwob_out: &mut ::std::vec::Vec<u8>) {
                __fwob_out.clear();
                __fwob_out.reserve(#frame_len);
                #(#encoders)*
            }

            fn decode(__fwob_bytes: &[u8]) -> ::fwob_core::Result<Self> {
                let __fwob_schema = Self::schema();
                __fwob_schema.validate_frame_len(__fwob_bytes.len())?;
                let mut __fwob_offset = 0usize;
                #(#decoders)*
                let _ = __fwob_offset;
                Ok(Self {
                    #(#initializers),*
                })
            }
        }
    })
}

struct FieldInfo {
    field_type: proc_macro2::TokenStream,
    length: u16,
    kind: FieldKind,
}

enum FieldKind {
    Primitive(Box<Type>),
    ByteArray(usize),
    Decimal,
    FixedString(usize),
    StringIndex8,
    StringIndex16,
    StringIndex32,
    StringIndex64,
}

impl FieldInfo {
    fn encode(&self, ident: &syn::Ident) -> proc_macro2::TokenStream {
        match &self.kind {
            FieldKind::Primitive(ty) if is_one_byte(ty) => {
                quote!(__fwob_out.push(self.#ident as u8);)
            }
            FieldKind::Primitive(_) => {
                quote!(__fwob_out.extend_from_slice(&self.#ident.to_le_bytes());)
            }
            FieldKind::ByteArray(_) => quote!(__fwob_out.extend_from_slice(&self.#ident);),
            FieldKind::Decimal => quote!(::fwob_core::encode_decimal(self.#ident, __fwob_out);),
            FieldKind::FixedString(_) => {
                quote!(__fwob_out.extend_from_slice(self.#ident.padded_bytes());)
            }
            FieldKind::StringIndex8 => quote!(__fwob_out.push(self.#ident.0);),
            FieldKind::StringIndex16 | FieldKind::StringIndex32 | FieldKind::StringIndex64 => {
                quote!(__fwob_out.extend_from_slice(&self.#ident.0.to_le_bytes());)
            }
        }
    }

    fn decode(&self, local: &syn::Ident) -> proc_macro2::TokenStream {
        let length = self.length as usize;
        match &self.kind {
            FieldKind::Primitive(ty) if is_one_byte(ty) => quote! {
                let #local = __fwob_bytes[__fwob_offset] as #ty;
                __fwob_offset += 1;
            },
            FieldKind::Primitive(ty) => quote! {
                let #local = #ty::from_le_bytes(
                    __fwob_bytes[__fwob_offset..__fwob_offset + #length]
                        .try_into()
                        .expect("validated frame length")
                );
                __fwob_offset += #length;
            },
            FieldKind::ByteArray(array_len) => quote! {
                let #local: [u8; #array_len] =
                    __fwob_bytes[__fwob_offset..__fwob_offset + #array_len]
                        .try_into()
                        .expect("validated frame length");
                __fwob_offset += #array_len;
            },
            FieldKind::Decimal => quote! {
                let #local = ::fwob_core::decode_decimal(
                    &__fwob_bytes[__fwob_offset..__fwob_offset + 16]
                )?;
                __fwob_offset += 16;
            },
            FieldKind::FixedString(string_len) => quote! {
                let #local = ::fwob_core::FixedString::<#string_len>::from_padded_bytes(
                    __fwob_bytes[__fwob_offset..__fwob_offset + #string_len]
                        .try_into()
                        .expect("validated frame length")
                )?;
                __fwob_offset += #string_len;
            },
            FieldKind::StringIndex8 => quote! {
                let #local = ::fwob_core::StringIndex8(__fwob_bytes[__fwob_offset]);
                __fwob_offset += 1;
            },
            FieldKind::StringIndex16 => quote! {
                let #local = ::fwob_core::StringIndex16(u16::from_le_bytes(
                    __fwob_bytes[__fwob_offset..__fwob_offset + 2]
                        .try_into()
                        .expect("validated frame length")
                ));
                __fwob_offset += 2;
            },
            FieldKind::StringIndex32 => quote! {
                let #local = ::fwob_core::StringIndex(u32::from_le_bytes(
                    __fwob_bytes[__fwob_offset..__fwob_offset + 4]
                        .try_into()
                        .expect("validated frame length")
                ));
                __fwob_offset += 4;
            },
            FieldKind::StringIndex64 => quote! {
                let #local = ::fwob_core::StringIndex64(u64::from_le_bytes(
                    __fwob_bytes[__fwob_offset..__fwob_offset + 8]
                        .try_into()
                        .expect("validated frame length")
                ));
                __fwob_offset += 8;
            },
        }
    }
}

fn field_info(ty: &Type, string_index: bool) -> syn::Result<FieldInfo> {
    if string_index {
        return match type_name(ty).as_deref() {
            Some("StringIndex8") => Ok(FieldInfo {
                field_type: quote!(::fwob_core::FieldType::StringTableIndex),
                length: 1,
                kind: FieldKind::StringIndex8,
            }),
            Some("StringIndex16") => Ok(FieldInfo {
                field_type: quote!(::fwob_core::FieldType::StringTableIndex),
                length: 2,
                kind: FieldKind::StringIndex16,
            }),
            Some("StringIndex") => Ok(FieldInfo {
                field_type: quote!(::fwob_core::FieldType::StringTableIndex),
                length: 4,
                kind: FieldKind::StringIndex32,
            }),
            Some("StringIndex64") => Ok(FieldInfo {
                field_type: quote!(::fwob_core::FieldType::StringTableIndex),
                length: 8,
                kind: FieldKind::StringIndex64,
            }),
            _ => Err(Error::new_spanned(
                ty,
                "#[fwob(string_index)] requires StringIndex8, StringIndex16, StringIndex, or StringIndex64",
            )),
        };
    }

    if let Some(length) = fixed_string_length(ty)? {
        return Ok(FieldInfo {
            field_type: quote!(::fwob_core::FieldType::Utf8String),
            length,
            kind: FieldKind::FixedString(length as usize),
        });
    }

    if let Type::Array(array) = ty {
        if type_name(&array.elem).as_deref() != Some("u8") {
            return Err(Error::new_spanned(
                array,
                "only [u8; N] arrays are supported",
            ));
        }
        let Type::Array(_) = ty else { unreachable!() };
        let syn::Expr::Lit(length) = &array.len else {
            return Err(Error::new_spanned(
                &array.len,
                "array length must be literal",
            ));
        };
        let syn::Lit::Int(length) = &length.lit else {
            return Err(Error::new_spanned(
                &array.len,
                "array length must be integer",
            ));
        };
        let length = parse_length(length)?;
        return Ok(FieldInfo {
            field_type: quote!(::fwob_core::FieldType::Utf8String),
            length,
            kind: FieldKind::ByteArray(length as usize),
        });
    }

    let Some(name) = type_name(ty) else {
        return Err(Error::new_spanned(ty, "unsupported FWOB field type"));
    };
    if name == "Decimal" {
        return Ok(FieldInfo {
            field_type: quote!(::fwob_core::FieldType::FloatingPoint),
            length: 16,
            kind: FieldKind::Decimal,
        });
    }
    let (field_type, length) = match name.as_str() {
        "i8" => (quote!(::fwob_core::FieldType::SignedInteger), 1),
        "i16" => (quote!(::fwob_core::FieldType::SignedInteger), 2),
        "i32" => (quote!(::fwob_core::FieldType::SignedInteger), 4),
        "i64" => (quote!(::fwob_core::FieldType::SignedInteger), 8),
        "u8" => (quote!(::fwob_core::FieldType::UnsignedInteger), 1),
        "u16" => (quote!(::fwob_core::FieldType::UnsignedInteger), 2),
        "u32" => (quote!(::fwob_core::FieldType::UnsignedInteger), 4),
        "u64" => (quote!(::fwob_core::FieldType::UnsignedInteger), 8),
        "f32" => (quote!(::fwob_core::FieldType::FloatingPoint), 4),
        "f64" => (quote!(::fwob_core::FieldType::FloatingPoint), 8),
        "StringIndex8" | "StringIndex16" | "StringIndex" | "StringIndex64" => {
            return Err(Error::new_spanned(
                ty,
                "string index wrappers require #[fwob(string_index)]",
            ))
        }
        _ => return Err(Error::new_spanned(ty, "unsupported FWOB field type")),
    };
    Ok(FieldInfo {
        field_type,
        length,
        kind: FieldKind::Primitive(Box::new(ty.clone())),
    })
}

fn type_name(ty: &Type) -> Option<String> {
    let Type::Path(path) = ty else {
        return None;
    };
    if !matches!(path.path.segments.last()?.arguments, PathArguments::None) {
        return None;
    }
    Some(path.path.segments.last()?.ident.to_string())
}

fn fixed_string_length(ty: &Type) -> syn::Result<Option<u16>> {
    let Type::Path(path) = ty else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != "FixedString" {
        return Ok(None);
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(Error::new_spanned(ty, "FixedString requires one length"));
    };
    if arguments.args.len() != 1 {
        return Err(Error::new_spanned(
            ty,
            "FixedString requires one integer literal length",
        ));
    }
    let Some(GenericArgument::Const(syn::Expr::Lit(length))) = arguments.args.first() else {
        return Err(Error::new_spanned(
            ty,
            "FixedString requires one integer literal length",
        ));
    };
    let syn::Lit::Int(length) = &length.lit else {
        return Err(Error::new_spanned(
            ty,
            "FixedString length must be an integer literal",
        ));
    };
    parse_length(length).map(Some)
}

fn is_one_byte(ty: &Type) -> bool {
    matches!(type_name(ty).as_deref(), Some("i8" | "u8"))
}

fn parse_length(length: &LitInt) -> syn::Result<u16> {
    let value = length.base10_parse::<usize>()?;
    let value =
        u16::try_from(value).map_err(|_| Error::new_spanned(length, "field is too large"))?;
    if value == 0 {
        Err(Error::new_spanned(length, "field length must be positive"))
    } else {
        Ok(value)
    }
}
