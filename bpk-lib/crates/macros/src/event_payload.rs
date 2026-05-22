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

    // ─── Parse keys: category + type_id, exactly once each, no unknowns ──────
    let mut category_lit: Option<LitInt> = None;
    let mut type_id_lit: Option<LitInt> = None;

    attr.parse_nested_meta(|meta| {
        let ident = meta
            .path
            .get_ident()
            .ok_or_else(|| meta.error("expected `category` or `type_id`"))?;
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
            other => {
                return Err(meta.error(format!(
                    "unknown key `{other}`, expected `category` or `type_id`"
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
    // justifies: INV-MACRO-BOUNDED-CAST; narrowing u64 to u8 is bounds-checked by the u8::MAX comparison on the preceding lines in crates/macros/src/event_payload.rs so truncation cannot occur here.
    #[allow(clippy::cast_possible_truncation)]
    let category: u8 = category_u64 as u8;
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
    // justifies: INV-MACRO-BOUNDED-CAST; narrowing u64 to u16 is bounds-checked by the u16::MAX comparison on the preceding lines in crates/macros/src/event_payload.rs so truncation cannot occur here.
    #[allow(clippy::cast_possible_truncation)]
    let type_id: u16 = type_id_u64 as u16;
    if let Err(msg) = batpak_macros_support::validate_type_id(type_id) {
        return Err(syn::Error::new(type_id_lit.span(), msg));
    }

    // ─── Codegen ─────────────────────────────────────────────────────────────
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    // Must agree byte-for-byte with EventKind::as_raw_u16. The proc macro runs
    // at expansion time before EventKind is available in this expression, so
    // the packed formula is intentionally inlined here.
    let kind_bits: u16 = (u16::from(category) << 12) | type_id;
    let test_fn_name = format_ident!("__batpak_kind_collision_check_{}", ident);

    // The emitted test fn is named `__batpak_kind_collision_check_<Ident>`
    // (CamelCase ident embedded), so `non_snake_case` has to be suppressed on
    // that specific item. The registration block is unconditional so payloads
    // in dependency crates remain visible to a downstream binary's explicit
    // registry validator.
    Ok(quote! {
        impl #impl_generics ::batpak::event::EventPayload for #ident #ty_generics #where_clause {
            const KIND: ::batpak::event::EventKind =
                ::batpak::event::EventKind::custom(#category, #type_id);
        }

        const _: () = {
            ::batpak::__private::inventory::submit! {
                ::batpak::__private::EventPayloadRegistration {
                    kind_bits: #kind_bits,
                    type_name: concat!(module_path!(), "::", stringify!(#ident)),
                }
            }
        };

        #[cfg(test)]
        #[test]
        // justifies: INV-GENERATED-WITNESS-PIN; generated test fn in crates/macros/src/event_payload.rs embeds the user's CamelCase ident so non_snake_case must be suppressed on this specific item.
        #[allow(non_snake_case)]
        fn #test_fn_name() {
            ::batpak::__private::assert_no_kind_collisions();
        }
    })
}
