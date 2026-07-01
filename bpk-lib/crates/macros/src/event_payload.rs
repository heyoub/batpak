use quote::{format_ident, quote};
use syn::{spanned::Spanned, Attribute, Data, DeriveInput, Fields, LitInt};

pub(super) fn expand(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[derive(EventPayload)] does not support generic payload types; use a concrete named-field struct",
        ));
    }

    // ─── Shape check: named-field struct only ────────────────────────────────
    let fields = match &input.data {
        Data::Struct(s) => &s.fields,
        Data::Enum(e) => {
            return Err(syn::Error::new(
                e.enum_token.span,
                "#[derive(EventPayload)] requires a named-field struct; enums are not supported",
            ));
        }
        Data::Union(u) => {
            return Err(syn::Error::new(
                u.union_token.span,
                "#[derive(EventPayload)] requires a named-field struct; unions are not supported",
            ));
        }
    };

    match fields {
        Fields::Named(_) => {}
        Fields::Unnamed(f) => {
            return Err(syn::Error::new(
                f.span(),
                "#[derive(EventPayload)] requires a named-field struct; tuple structs are not supported",
            ));
        }
        Fields::Unit => {
            return Err(syn::Error::new(
                input.ident.span(),
                "#[derive(EventPayload)] requires a named-field struct; unit structs are not supported",
            ));
        }
    }

    // ─── Attribute: exactly one #[batpak(...)] ───────────────────────────────
    let batpak_attrs: Vec<&Attribute> = input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("batpak"))
        .collect();

    let attr = match batpak_attrs.as_slice() {
        [] => {
            return Err(syn::Error::new(
                input.ident.span(),
                "#[derive(EventPayload)] requires a `#[batpak(category = N, type_id = N)]` attribute",
            ));
        }
        [a] => *a,
        [_, second, ..] => {
            return Err(syn::Error::new(
                second.span(),
                "expected exactly one `#[batpak(...)]` attribute",
            ));
        }
    };

    // ─── Parse keys: category + type_id (required), version (optional) ──────
    // `version` is the wire payload schema version. It defaults to 1 when
    // absent, matching `EventPayload::PAYLOAD_VERSION`'s associated-const
    // default, so existing derives keep emitting version 1 with no edit.
    let mut category_lit: Option<LitInt> = None;
    let mut type_id_lit: Option<LitInt> = None;
    let mut version_lit: Option<LitInt> = None;

    attr.parse_nested_meta(|meta| {
        let ident = meta
            .path
            .get_ident()
            .ok_or_else(|| meta.error("expected `category`, `type_id`, or `version`"))?;
        match ident.to_string().as_str() {
            "category" => {
                if category_lit.is_some() {
                    return Err(meta.error("duplicate `category` key"));
                }
                category_lit = Some(meta.value()?.parse::<LitInt>()?);
            }
            "type_id" => {
                if type_id_lit.is_some() {
                    return Err(meta.error("duplicate `type_id` key"));
                }
                type_id_lit = Some(meta.value()?.parse::<LitInt>()?);
            }
            "version" => {
                if version_lit.is_some() {
                    return Err(meta.error("duplicate `version` key"));
                }
                version_lit = Some(meta.value()?.parse::<LitInt>()?);
            }
            other => {
                return Err(meta.error(format!(
                    "unknown key `{other}`, expected `category`, `type_id`, or `version`"
                )));
            }
        }
        Ok(())
    })?;

    let category_lit = category_lit
        .ok_or_else(|| syn::Error::new(attr.span(), "`#[batpak(...)]` requires `category = N`"))?;
    let type_id_lit = type_id_lit
        .ok_or_else(|| syn::Error::new(attr.span(), "`#[batpak(...)]` requires `type_id = N`"))?;

    // ─── Value validation: parse wide, then narrow + check reserved ranges ──
    // The local guards reject values outside the narrow integer width so the
    // `as u8` / `as u16` casts cannot silently truncate. The actual 4-bit /
    // 12-bit + reserved-value constraint lives in `validate_category` /
    // `validate_type_id` below; surface both in the error so a caller writing
    // `category = 0x100` is not told to "fit in 4 bits" without context.
    let category_u64: u64 = category_lit.base10_parse()?;
    if category_u64 > u64::from(u8::MAX) {
        return Err(syn::Error::new(
            category_lit.span(),
            format!(
                "category {category_u64:#x} exceeds u8 range; \
                 category must fit in 4 bits (0x1–0xF, excluding 0x0 and 0xD)"
            ),
        ));
    }
    // The preceding `> u64::from(u8::MAX)` guard makes this conversion total;
    // `try_from` keeps it lint-clean instead of an unchecked `as` narrowing.
    let category: u8 =
        u8::try_from(category_u64).expect("category bounded to u8 range by guard above");
    if let Err(msg) = batpak_macros_support::validate_category(category) {
        return Err(syn::Error::new(category_lit.span(), msg));
    }

    let type_id_u64: u64 = type_id_lit.base10_parse()?;
    if type_id_u64 > u64::from(u16::MAX) {
        return Err(syn::Error::new(
            type_id_lit.span(),
            format!(
                "type_id {type_id_u64:#x} exceeds u16 range; \
                 type_id must fit in 12 bits (0x000–0xFFF)"
            ),
        ));
    }
    // The preceding `> u64::from(u16::MAX)` guard makes this conversion total.
    let type_id: u16 =
        u16::try_from(type_id_u64).expect("type_id bounded to u16 range by guard above");
    if let Err(msg) = batpak_macros_support::validate_type_id(type_id) {
        return Err(syn::Error::new(type_id_lit.span(), msg));
    }

    // `version` defaults to 1. A `version = 0` literal is rejected: 0 is the
    // legacy/untyped sentinel on the wire (`EventHeader.payload_version`), so a
    // declared payload may never claim it.
    let payload_version: u16 = match &version_lit {
        None => 1,
        Some(lit) => {
            let version_u64: u64 = lit.base10_parse()?;
            if version_u64 == 0 {
                return Err(syn::Error::new(
                    lit.span(),
                    "version 0 is reserved for legacy/untyped frames; declared payloads start at version 1",
                ));
            }
            if version_u64 > u64::from(u16::MAX) {
                return Err(syn::Error::new(
                    lit.span(),
                    format!(
                        "version {version_u64} exceeds u16 range; payload version must fit in u16"
                    ),
                ));
            }
            // The preceding `> u64::from(u16::MAX)` guard makes this total.
            u16::try_from(version_u64).expect("version bounded to u16 range by guard above")
        }
    };

    // ─── Codegen ─────────────────────────────────────────────────────────────
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    // Must agree byte-for-byte with EventKind::as_raw_u16. The proc macro runs
    // at expansion time before EventKind is available in this expression, so
    // the packed formula is intentionally inlined here.
    let kind_bits: u16 = (u16::from(category) << 12) | type_id;
    // Embed the user's ident in snake_case so the generated test fn is itself a
    // valid snake_case identifier and does not trip `non_snake_case` — no lint
    // suppression on generated code. The conversion is deterministic and unique
    // per source ident (idents are already unique within a module).
    let snake_ident = to_snake_case(&ident.to_string());
    let test_fn_name = format_ident!("__batpak_kind_collision_check_{}", snake_ident);

    // The registration block is unconditional so payloads in dependency crates
    // remain visible to a downstream binary's explicit registry validator.
    Ok(quote! {
        impl #impl_generics ::batpak::event::EventPayload for #ident #ty_generics #where_clause {
            const KIND: ::batpak::event::EventKind =
                ::batpak::event::EventKind::custom(#category, #type_id);
            const PAYLOAD_VERSION: u16 = #payload_version;
        }

        const _: () = {
            ::batpak::__private::inventory::submit! {
                ::batpak::__private::EventPayloadRegistration {
                    kind_bits: #kind_bits, payload_version: #payload_version,
                    type_name: concat!(module_path!(), "::", stringify!(#ident)),
                }
            }
        };

        #[cfg(test)]
        #[test]
        fn #test_fn_name() {
            ::batpak::__private::assert_no_kind_collisions();
        }
    })
}

/// Convert a (typically CamelCase) Rust identifier into `snake_case`.
///
/// Used only to build a valid snake_case generated test-fn name from the user's
/// payload type ident, so the emitted item needs no `non_snake_case` allow.
fn to_snake_case(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 4);
    let mut prev_lower_or_digit = false;
    for ch in ident.chars() {
        if ch.is_ascii_uppercase() {
            if prev_lower_or_digit {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            prev_lower_or_digit = false;
        } else {
            out.push(ch);
            prev_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        }
    }
    out
}
