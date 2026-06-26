//! `#[operation]` attribute-macro implementation.
//!
//! Generates a syncbat operation descriptor and optional registration fns. The
//! emitted code references `::syncbat::...` paths, but this module compile-depends
//! only on syn/quote/proc-macro2 — it carries no syncbat edge. (Lives here, in the
//! one family proc-macro crate, rather than a separate `syncbat-macros` crate;
//! the macro layer is one crate.)

use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Error, Expr, ExprLit, FnArg, Ident, ItemFn, Lit, MetaNameValue, Result, Token};

pub(crate) struct OperationArgs {
    pairs: Punctuated<MetaNameValue, Token![,]>,
}

impl Parse for OperationArgs {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            pairs: Punctuated::parse_terminated(input)?,
        })
    }
}

struct ParsedOperationArgs {
    descriptor: Ident,
    register: Option<Ident>,
    register_item: Option<Ident>,
    name: Lit,
    effect: Ident,
    input_schema: Lit,
    output_schema: Lit,
    receipt_kind: Lit,
    title: Option<Lit>,
    reads_events: Vec<Lit>,
    appends_events: Vec<Lit>,
    queries_projections: Vec<Lit>,
    emits_receipts: Vec<Lit>,
    uses_host_controls: bool,
    requires_capabilities: Vec<Lit>,
}

#[derive(Default)]
struct OperationArgSlots {
    descriptor: Option<Ident>,
    register: Option<Ident>,
    register_item: Option<Ident>,
    name: Option<Lit>,
    effect: Option<Ident>,
    input_schema: Option<Lit>,
    output_schema: Option<Lit>,
    receipt_kind: Option<Lit>,
    title: Option<Lit>,
    reads_events: Option<Vec<Lit>>,
    appends_events: Option<Vec<Lit>>,
    queries_projections: Option<Vec<Lit>>,
    emits_receipts: Option<Vec<Lit>>,
    uses_host_controls: Option<bool>,
    requires_capabilities: Option<Vec<Lit>>,
}

pub(crate) fn expand_operation(
    args: OperationArgs,
    function: &ItemFn,
) -> Result<proc_macro2::TokenStream> {
    validate_function(function)?;
    let parsed = parse_args(args)?;

    let fn_name = &function.sig.ident;
    let descriptor = &parsed.descriptor;
    let name = &parsed.name;
    let effect = &parsed.effect;
    let input_schema = &parsed.input_schema;
    let output_schema = &parsed.output_schema;
    let receipt_kind = &parsed.receipt_kind;
    let descriptor_base_expr = if let Some(title) = &parsed.title {
        quote! {
            ::syncbat::OperationDescriptor::new_with_title(
                #name,
                ::syncbat::EffectClass::#effect,
                #input_schema,
                #output_schema,
                #receipt_kind,
                #title,
            )
        }
    } else {
        quote! {
            ::syncbat::OperationDescriptor::new(
                #name,
                ::syncbat::EffectClass::#effect,
                #input_schema,
                #output_schema,
                #receipt_kind,
            )
        }
    };
    let effect_row_expr = build_effect_row_expr(&parsed);
    let descriptor_has_effect_row = parsed.has_effect_row();
    let descriptor_expr = if descriptor_has_effect_row {
        quote! {
            #descriptor_base_expr.with_effect_row(#effect_row_expr)
        }
    } else {
        descriptor_base_expr
    };
    let descriptor_decl = if descriptor_has_effect_row {
        quote! {
            static #descriptor: ::std::sync::LazyLock<::syncbat::OperationDescriptor> =
                ::std::sync::LazyLock::new(|| #descriptor_expr);
        }
    } else {
        quote! {
            const #descriptor: ::syncbat::OperationDescriptor = #descriptor_expr;
        }
    };
    let descriptor_clone_expr = if descriptor_has_effect_row {
        quote! { ::std::clone::Clone::clone(&*#descriptor) }
    } else {
        quote! { #descriptor.clone() }
    };

    let register_item_fn = parsed.register_item.as_ref().map(|register_item| {
        quote! {
            pub fn #register_item() -> ::syncbat::OperationRegisterItem {
                ::syncbat::OperationRegisterItem::new(#descriptor_clone_expr, #fn_name)
            }
        }
    });

    let item_expr = if let Some(register_item) = &parsed.register_item {
        quote! { #register_item() }
    } else {
        quote! { ::syncbat::OperationRegisterItem::new(#descriptor_clone_expr, #fn_name) }
    };

    let register_fn = parsed.register.map(|register| {
        quote! {
            pub fn #register(
                builder: &mut ::syncbat::CoreBuilder,
            ) -> ::std::result::Result<&mut ::syncbat::CoreBuilder, ::syncbat::BuildError> {
                builder.register_item(#item_expr)
            }
        }
    });

    Ok(quote! {
        #function

        #descriptor_decl

        const _: fn(&[u8], &mut ::syncbat::Ctx<'_>) -> ::syncbat::HandlerResult = #fn_name;

        #register_item_fn

        #register_fn
    })
}

fn validate_function(function: &ItemFn) -> Result<()> {
    if let Some(asyncness) = &function.sig.asyncness {
        return Err(Error::new(
            asyncness.span,
            "#[syncbat::operation] does not support async functions",
        ));
    }
    if let Some(unsafety) = &function.sig.unsafety {
        return Err(Error::new(
            unsafety.span,
            "#[syncbat::operation] does not support unsafe functions",
        ));
    }
    if let Some(abi) = &function.sig.abi {
        let is_rust_abi = abi.name.as_ref().is_some_and(|name| name.value() == "Rust");
        if !is_rust_abi {
            return Err(Error::new(
                abi.extern_token.span,
                "#[syncbat::operation] only supports Rust ABI functions",
            ));
        }
    }
    if !function.sig.generics.params.is_empty() || function.sig.generics.where_clause.is_some() {
        return Err(Error::new(
            function.sig.generics.span(),
            "#[syncbat::operation] does not support generic functions",
        ));
    }

    if function.sig.inputs.len() != 2 {
        return Err(Error::new(
            function.sig.inputs.span(),
            "#[syncbat::operation] handlers must accept `&[u8]` and `&mut syncbat::Ctx<'_>`",
        ));
    }
    if function
        .sig
        .inputs
        .iter()
        .any(|arg| matches!(arg, FnArg::Receiver(_)))
    {
        return Err(Error::new(
            function.sig.inputs.span(),
            "#[syncbat::operation] handlers must be free functions",
        ));
    }

    Ok(())
}

fn parse_args(args: OperationArgs) -> Result<ParsedOperationArgs> {
    let mut slots = OperationArgSlots::default();
    for pair in args.pairs {
        slots.set_pair(&pair)?;
    }

    slots.finish()
}

impl OperationArgSlots {
    fn set_pair(&mut self, pair: &MetaNameValue) -> Result<()> {
        let key = pair
            .path
            .get_ident()
            .ok_or_else(|| Error::new(pair.path.span(), "expected operation attribute key"))?;
        let key_name = key.to_string();
        if self.set_core_pair(key_name.as_str(), pair)? {
            return Ok(());
        }
        if self.set_effect_pair(key_name.as_str(), pair)? {
            return Ok(());
        }
        Err(Error::new(
            pair.path.span(),
            format!("unknown key `{key_name}` in #[syncbat::operation]"),
        ))
    }

    fn set_core_pair(&mut self, key: &str, pair: &MetaNameValue) -> Result<bool> {
        match key {
            "descriptor" => set_ident(&mut self.descriptor, "descriptor", pair)?,
            "register" => set_ident(&mut self.register, "register", pair)?,
            "register_item" => set_ident(&mut self.register_item, "register_item", pair)?,
            "name" => set_string(&mut self.name, "name", pair)?,
            "effect" => set_effect(&mut self.effect, pair)?,
            "input_schema" => set_string(&mut self.input_schema, "input_schema", pair)?,
            "output_schema" => set_string(&mut self.output_schema, "output_schema", pair)?,
            "receipt_kind" => set_string(&mut self.receipt_kind, "receipt_kind", pair)?,
            "title" => set_string(&mut self.title, "title", pair)?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_effect_pair(&mut self, key: &str, pair: &MetaNameValue) -> Result<bool> {
        match key {
            "reads_events" => set_string_list(&mut self.reads_events, "reads_events", pair)?,
            "appends_events" => set_string_list(&mut self.appends_events, "appends_events", pair)?,
            "queries_projections" => {
                set_string_list(&mut self.queries_projections, "queries_projections", pair)?
            }
            "emits_receipts" => set_string_list(&mut self.emits_receipts, "emits_receipts", pair)?,
            "uses_host_controls" => {
                set_bool(&mut self.uses_host_controls, "uses_host_controls", pair)?
            }
            "requires_capabilities" => set_string_list(
                &mut self.requires_capabilities,
                "requires_capabilities",
                pair,
            )?,
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn finish(self) -> Result<ParsedOperationArgs> {
        Ok(ParsedOperationArgs {
            descriptor: required(self.descriptor, "descriptor")?,
            register: self.register,
            register_item: self.register_item,
            name: required(self.name, "name")?,
            effect: required(self.effect, "effect")?,
            input_schema: required(self.input_schema, "input_schema")?,
            output_schema: required(self.output_schema, "output_schema")?,
            receipt_kind: required(self.receipt_kind, "receipt_kind")?,
            title: self.title,
            reads_events: self.reads_events.unwrap_or_default(),
            appends_events: self.appends_events.unwrap_or_default(),
            queries_projections: self.queries_projections.unwrap_or_default(),
            emits_receipts: self.emits_receipts.unwrap_or_default(),
            uses_host_controls: self.uses_host_controls.unwrap_or(false),
            requires_capabilities: self.requires_capabilities.unwrap_or_default(),
        })
    }
}

impl ParsedOperationArgs {
    fn has_effect_row(&self) -> bool {
        !self.reads_events.is_empty()
            || !self.appends_events.is_empty()
            || !self.queries_projections.is_empty()
            || !self.emits_receipts.is_empty()
            || self.uses_host_controls
            || !self.requires_capabilities.is_empty()
    }
}

fn build_effect_row_expr(parsed: &ParsedOperationArgs) -> proc_macro2::TokenStream {
    let mut row = quote! { ::syncbat::OperationEffectRow::new() };
    for target in &parsed.reads_events {
        row = quote! { #row.reads_event(#target) };
    }
    for target in &parsed.appends_events {
        row = quote! { #row.appends_event(#target) };
    }
    for target in &parsed.queries_projections {
        row = quote! { #row.queries_projection(#target) };
    }
    for target in &parsed.emits_receipts {
        row = quote! { #row.emits_receipt(#target) };
    }
    if parsed.uses_host_controls {
        row = quote! { #row.uses_host_control() };
    }
    for target in &parsed.requires_capabilities {
        row = quote! { #row.requires_capability(#target) };
    }
    row
}

fn set_ident(target: &mut Option<Ident>, key: &str, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            format!("duplicate `{key}` key in #[syncbat::operation]"),
        ));
    }
    if let Expr::Path(path) = &pair.value {
        if path.path.segments.len() == 1 && path.path.get_ident().is_some() {
            *target = path.path.get_ident().cloned();
            return Ok(());
        }
    }
    Err(Error::new(
        pair.value.span(),
        format!("`{key}` must be a Rust identifier"),
    ))
}

fn set_string(target: &mut Option<Lit>, key: &str, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            format!("duplicate `{key}` key in #[syncbat::operation]"),
        ));
    }
    match string_lit(&pair.value) {
        Some(lit) => {
            *target = Some(lit);
            Ok(())
        }
        None => Err(Error::new(
            pair.value.span(),
            format!("`{key}` must be a string literal"),
        )),
    }
}

fn set_string_list(target: &mut Option<Vec<Lit>>, key: &str, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            format!("duplicate `{key}` key in #[syncbat::operation]"),
        ));
    }
    let Expr::Array(array) = &pair.value else {
        return Err(Error::new(
            pair.value.span(),
            format!("`{key}` must be an array of string literals"),
        ));
    };
    let mut values = Vec::new();
    for element in &array.elems {
        match string_lit(element) {
            Some(lit) => values.push(lit),
            None => {
                return Err(Error::new(
                    element.span(),
                    format!("`{key}` entries must be string literals"),
                ));
            }
        }
    }
    *target = Some(values);
    Ok(())
}

fn set_bool(target: &mut Option<bool>, key: &str, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            format!("duplicate `{key}` key in #[syncbat::operation]"),
        ));
    }
    if let Expr::Lit(ExprLit {
        lit: Lit::Bool(value),
        ..
    }) = &pair.value
    {
        *target = Some(value.value);
        return Ok(());
    }
    Err(Error::new(
        pair.value.span(),
        format!("`{key}` must be a bool literal"),
    ))
}

fn set_effect(target: &mut Option<Ident>, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            "duplicate `effect` key in #[syncbat::operation]",
        ));
    }
    if let Expr::Path(path) = &pair.value {
        if path.path.segments.len() == 1 {
            if let Some(ident) = path.path.get_ident() {
                return match ident.to_string().as_str() {
                    "Inspect" | "Compute" | "Persist" | "Emit" | "Control" => {
                        *target = Some(ident.clone());
                        Ok(())
                    }
                    other => Err(Error::new(
                        ident.span(),
                        format!("unsupported effect `{other}` in #[syncbat::operation]"),
                    )),
                };
            }
        }
    }
    Err(Error::new(
        pair.value.span(),
        "`effect` must be a syncbat EffectClass variant identifier",
    ))
}

fn string_lit(expr: &Expr) -> Option<Lit> {
    if let Expr::Lit(ExprLit {
        lit: lit @ Lit::Str(_),
        ..
    }) = expr
    {
        Some(lit.clone())
    } else {
        None
    }
}

fn required<T>(value: Option<T>, key: &str) -> Result<T> {
    value.ok_or_else(|| {
        Error::new(
            proc_macro2::Span::call_site(),
            format!("#[syncbat::operation] requires `{key} = ...`"),
        )
    })
}
