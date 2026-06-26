//! Proc macros for the batpak event-sourcing runtime.
//!
//! This crate is pulled in transitively via `batpak`. Users never add it
//! to their own `Cargo.toml` — the derives are already in scope via
//! `use batpak::EventPayload;` or `use batpak::EventSourced;`.

mod event_payload;
mod operation;

use proc_macro::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{
    parse_macro_input, spanned::Spanned, Attribute, Data, DeriveInput, Fields, Ident, LitInt, Path,
};

/// Derives `batpak::event::EventPayload` for a named-field struct.
///
/// Requires `#[batpak(category = N, type_id = N)]` on the struct. See
/// `batpak::event::EventPayload` and ADR-0010 for the full contract.
#[proc_macro_derive(EventPayload, attributes(batpak))]
pub fn derive_event_payload(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match event_payload::expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// `#[operation(...)]` — generate a syncbat operation descriptor + optional
/// registration fns. Re-exported as `syncbat::operation`; users never name this
/// crate. (Moved here from the former `syncbat-macros` crate — the family has one
/// proc-macro crate.)
#[proc_macro_attribute]
pub fn operation(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as operation::OperationArgs);
    let function = parse_macro_input!(item as syn::ItemFn);
    match operation::expand_operation(args, &function) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derives `batpak::event::MultiReactive<Input>` for a named-field struct,
/// for use with `Store::react_loop_multi` (JSON) or
/// `Store::react_loop_multi_raw` (msgpack).
///
/// Syntax mirrors `#[derive(EventSourced)]`:
///   * `#[batpak(input = <Lane>)]` — required, once. `Lane` is either
///     `JsonValueInput` or `RawMsgpackInput`.
///   * `#[batpak(event = <Payload>, handler = <fn>)]` — one per bound
///     payload type. At least one is required. `event = T` requires `T` to
///     be a single-segment path (bring the type into scope with `use` if
///     needed). This ensures the derive can dedupe event bindings without
///     running full path resolution.
///
/// Generates a `MultiReactive<Input>` impl whose `dispatch` body matches
/// on `event.header.event_kind`, uses `DecodeTyped::route_typed` per arm,
/// calls the matching handler, and returns `MultiDispatchError::Decode` on
/// matched-kind decode failure (unified contract with `TypedReactive<T>`).
/// Unbound kinds fall through as `Ok(())` — silent filter.
#[proc_macro_derive(MultiEventReactor, attributes(batpak))]
pub fn derive_multi_event_reactor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_multi_event_reactor(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derives `batpak::event::EventSourced` for a named-field struct.
///
/// Requires a config attr `#[batpak(input = <Lane>, cache_version = N)]`
/// (the `cache_version` key is optional and defaults to 0) plus at least
/// one event-binding attr `#[batpak(event = <Payload>, handler = <fn>)]`.
/// `event = T` requires `T` to be a single-segment path (bring the type into
/// scope with `use` if needed). This ensures the derive can dedupe event
/// bindings without running full path resolution.
///
/// Generates:
///   - `type Input = <Lane>`
///   - `from_events` — default fold over `Default::default()`
///   - `apply_event` — dispatch by `P::KIND` via `DecodeTyped::route_typed`,
///     with the two failure modes kept rigorously distinct:
///       * wrong-kind event → silent skip (fall-through to next arm)
///       * matched-kind + decode failure → `panic!` (see "Panics" below)
///   - `relevant_event_kinds` — `&[T1::KIND, T2::KIND, ...]` generated from
///     the `event =` list (single source of truth; sync-drift is impossible)
///   - `schema_version` — from `cache_version` (projection-cache invalidation
///     only; unrelated to payload wire `type_id`)
///
/// # Panics
///
/// The generated `apply_event` **panics** when an event's `event_kind` matches
/// a bound payload's `KIND` but the payload bytes fail to deserialize into
/// that payload type. This is a deliberate contract:
///
/// 1. The raw `EventSourced` trait's `apply_event` returns `()`, not `Result`.
///    A hand-written implementation must either panic, log-and-skip, or
///    log-and-ignore on decode failure. The canonical pattern demonstrated
///    in the pre-derive `examples/event_sourced_counter.rs` used
///    `.expect(...)`, which is equivalent.
///
/// 2. Matched-kind decode failure is a **hard correctness signal** — the
///    event was written as this kind but the bytes are malformed (schema
///    drift, `type_id` reuse, corruption). Silently skipping would produce
///    incorrect projected state.
///
/// If you need fallible replay (log-and-skip, fail-the-projection, custom
/// recovery), implement `EventSourced` manually. The derive does not offer a
/// fallible mode because the trait signature does not support one.
///
/// See `TERMINALS.md`, `CIRCUITS.md`, and the Dispatch Chapter plan
/// for the full contract.
#[proc_macro_derive(EventSourced, attributes(batpak))]
pub fn derive_event_sourced(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_event_sourced(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ─── EventSourced derive expansion ────────────────────────────────────────────

/// One `#[batpak(event = X, handler = fn)]` entry parsed from the derive
/// attrs.
struct EventBinding {
    event: Path,
    handler: Ident,
}

/// Parsed state for a single `#[batpak(...)]` attribute on an `EventSourced`
/// or `MultiEventReactor` derive. Each attribute is either a `Config` attr
/// (containing `input`, `cache_version`, `error`, or projection state
/// contract fields) or an `EventBinding` attr (containing `event` and
/// `handler`). Mixing keys is a compile-time error.
enum BatpakAttrKind {
    Config {
        input: Option<Path>,
        cache_version: Option<LitInt>,
        state_max_cardinality: Option<LitInt>,
        error: Option<Path>,
    },
    Event(EventBinding),
}

#[derive(Default)]
struct BatpakAttrParts {
    input: Option<Path>,
    cache_version: Option<LitInt>,
    state_max_cardinality: Option<LitInt>,
    error_ty: Option<Path>,
    event: Option<Path>,
    handler: Option<Ident>,
}

impl BatpakAttrParts {
    fn set_nested(&mut self, meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
        let key = meta.path.get_ident().ok_or_else(|| {
            meta.error("expected `input`, `cache_version`, `state_max_cardinality`, `error`, `event`, or `handler`")
        })?;
        let key_name = key.to_string();
        if self.set_config_nested(key_name.as_str(), meta)? {
            return Ok(());
        }
        if self.set_event_nested(key_name.as_str(), meta)? {
            return Ok(());
        }
        Err(meta.error(format!(
            "unknown key `{key_name}`, expected `input`, `cache_version`, `state_max_cardinality`, `error`, `event`, or `handler`"
        )))
    }

    fn set_config_nested(
        &mut self,
        key: &str,
        meta: &syn::meta::ParseNestedMeta<'_>,
    ) -> syn::Result<bool> {
        match key {
            "input" => {
                if self.input.is_some() {
                    return Err(meta.error("duplicate `input` key within attribute"));
                }
                self.input = Some(meta.value()?.parse::<Path>()?);
            }
            "cache_version" => {
                if self.cache_version.is_some() {
                    return Err(meta.error("duplicate `cache_version` key within attribute"));
                }
                self.cache_version = Some(meta.value()?.parse::<LitInt>()?);
            }
            "state_max_cardinality" => {
                if self.state_max_cardinality.is_some() {
                    return Err(
                        meta.error("duplicate `state_max_cardinality` key within attribute")
                    );
                }
                self.state_max_cardinality = Some(meta.value()?.parse::<LitInt>()?);
            }
            "error" => {
                if self.error_ty.is_some() {
                    return Err(meta.error("duplicate `error` key within attribute"));
                }
                self.error_ty = Some(meta.value()?.parse::<Path>()?);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn set_event_nested(
        &mut self,
        key: &str,
        meta: &syn::meta::ParseNestedMeta<'_>,
    ) -> syn::Result<bool> {
        match key {
            "event" => {
                if self.event.is_some() {
                    return Err(meta.error("duplicate `event` key within attribute"));
                }
                self.event = Some(meta.value()?.parse::<Path>()?);
            }
            "handler" => {
                if self.handler.is_some() {
                    return Err(meta.error("duplicate `handler` key within attribute"));
                }
                self.handler = Some(meta.value()?.parse::<Ident>()?);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn finish(self, attr: &Attribute) -> syn::Result<BatpakAttrKind> {
        let has_config = self.input.is_some()
            || self.cache_version.is_some()
            || self.state_max_cardinality.is_some()
            || self.error_ty.is_some();
        let has_event = self.event.is_some() || self.handler.is_some();

        if has_config && has_event {
            return Err(syn::Error::new(
                attr.span(),
                "`#[batpak(...)]` attribute must contain either config keys \
                 (`input`, `cache_version`, `state_max_cardinality`, `error`) or an event-binding pair (`event`, `handler`), not both",
            ));
        }

        if has_event {
            let event = self.event.ok_or_else(|| {
                syn::Error::new(
                    attr.span(),
                    "event-binding attribute is missing `event = <PayloadType>`",
                )
            })?;
            let handler = self.handler.ok_or_else(|| {
                syn::Error::new(
                    attr.span(),
                    "event-binding attribute is missing `handler = <fn_name>`",
                )
            })?;
            return Ok(BatpakAttrKind::Event(EventBinding { event, handler }));
        }

        if !has_config {
            return Err(syn::Error::new(
                attr.span(),
                "`#[batpak(...)]` must contain at least one key: `input`, `cache_version`, `state_max_cardinality`, `error`, or the `event`/`handler` pair",
            ));
        }
        Ok(BatpakAttrKind::Config {
            input: self.input,
            cache_version: self.cache_version,
            state_max_cardinality: self.state_max_cardinality,
            error: self.error_ty,
        })
    }
}

fn classify_batpak_attr(attr: &Attribute) -> syn::Result<BatpakAttrKind> {
    let mut parts = BatpakAttrParts::default();
    attr.parse_nested_meta(|meta| parts.set_nested(&meta))?;
    parts.finish(attr)
}

fn ensure_named_field_struct(input: &DeriveInput, derive_name: &str) -> syn::Result<()> {
    match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(_) => Ok(()),
            Fields::Unnamed(f) => Err(syn::Error::new(
                f.span(),
                format!(
                    "#[derive({derive_name})] requires a named-field struct; tuple structs are not supported"
                ),
            )),
            Fields::Unit => Err(syn::Error::new(
                input.ident.span(),
                format!(
                    "#[derive({derive_name})] requires a named-field struct; unit structs are not supported"
                ),
            )),
        },
        Data::Enum(e) => Err(syn::Error::new(
            e.enum_token.span,
            format!("#[derive({derive_name})] requires a named-field struct; enums are not supported"),
        )),
        Data::Union(u) => Err(syn::Error::new(
            u.union_token.span,
            format!(
                "#[derive({derive_name})] requires a named-field struct; unions are not supported"
            ),
        )),
    }
}

struct EventSourcedDeriveAttrs {
    input_path: Path,
    cache_version_lit: Option<LitInt>,
    state_max_cardinality_lit: Option<LitInt>,
    bindings: Vec<EventBinding>,
}

fn collect_event_sourced_attrs(input: &DeriveInput) -> syn::Result<EventSourcedDeriveAttrs> {
    let batpak_attrs: Vec<&Attribute> = input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("batpak"))
        .collect();

    if batpak_attrs.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[derive(EventSourced)] requires at least one `#[batpak(input = <Lane>)]` attribute",
        ));
    }

    let mut input_path: Option<Path> = None;
    let mut cache_version_lit: Option<LitInt> = None;
    let mut state_max_cardinality_lit: Option<LitInt> = None;
    let mut bindings: Vec<EventBinding> = Vec::new();
    let mut seen_events: HashSet<String> = HashSet::new();

    for attr in &batpak_attrs {
        match classify_batpak_attr(attr)? {
            BatpakAttrKind::Config {
                input: attr_input,
                cache_version: attr_cache,
                state_max_cardinality: attr_state_max,
                error: attr_error,
            } => {
                collect_event_sourced_config(
                    &mut input_path,
                    &mut cache_version_lit,
                    &mut state_max_cardinality_lit,
                    attr_input,
                    attr_cache,
                    attr_state_max,
                    attr_error,
                )?;
            }
            BatpakAttrKind::Event(binding) => {
                collect_unique_event_binding(
                    &mut bindings,
                    &mut seen_events,
                    binding,
                    "projection",
                )?;
            }
        }
    }

    let input_path = input_path.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "#[derive(EventSourced)] requires `#[batpak(input = <Lane>)]` — e.g. `input = JsonValueInput` or `input = RawMsgpackInput`",
        )
    })?;

    if bindings.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "`#[derive(EventSourced)]` requires at least one `#[batpak(event = T, handler = h)]` binding",
        ));
    }

    Ok(EventSourcedDeriveAttrs {
        input_path,
        cache_version_lit,
        state_max_cardinality_lit,
        bindings,
    })
}

fn collect_event_sourced_config(
    input_path: &mut Option<Path>,
    cache_version_lit: &mut Option<LitInt>,
    state_max_cardinality_lit: &mut Option<LitInt>,
    attr_input: Option<Path>,
    attr_cache: Option<LitInt>,
    attr_state_max: Option<LitInt>,
    attr_error: Option<Path>,
) -> syn::Result<()> {
    if let Some(path) = attr_error {
        return Err(syn::Error::new(
            path.span(),
            "`error` is not valid on `#[derive(EventSourced)]` — projections do not have an associated error type",
        ));
    }
    if let Some(path) = attr_input {
        if input_path.is_some() {
            return Err(syn::Error::new(
                path.span(),
                "duplicate `input =` across `#[batpak(...)]` config attributes — `input` must appear exactly once",
            ));
        }
        *input_path = Some(path);
    }
    if let Some(lit) = attr_cache {
        if cache_version_lit.is_some() {
            return Err(syn::Error::new(
                lit.span(),
                "duplicate `cache_version =` across `#[batpak(...)]` config attributes",
            ));
        }
        *cache_version_lit = Some(lit);
    }
    if let Some(lit) = attr_state_max {
        if state_max_cardinality_lit.is_some() {
            return Err(syn::Error::new(
                lit.span(),
                "duplicate `state_max_cardinality =` across `#[batpak(...)]` config attributes",
            ));
        }
        *state_max_cardinality_lit = Some(lit);
    }
    Ok(())
}

fn collect_unique_event_binding(
    bindings: &mut Vec<EventBinding>,
    seen_events: &mut HashSet<String>,
    binding: EventBinding,
    owner: &str,
) -> syn::Result<()> {
    require_single_segment_event_path(&binding.event)?;
    let key = binding.event.to_token_stream_string();
    if !seen_events.insert(key) {
        return Err(syn::Error::new(
            binding.event.span(),
            format!(
                "duplicate `event = X` — each payload type may be bound to exactly one handler per {owner}"
            ),
        ));
    }
    bindings.push(binding);
    Ok(())
}

fn expand_event_sourced(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    ensure_named_field_struct(input, "EventSourced")?;

    let attrs = collect_event_sourced_attrs(input)?;
    let input_path = attrs.input_path;
    let cache_version_lit = attrs.cache_version_lit;
    let state_max_cardinality_lit = attrs.state_max_cardinality_lit;
    let bindings = attrs.bindings;

    // ─── Validate cache_version fits u64 ────────────────────────────────────
    let cache_version_value: u64 = match &cache_version_lit {
        Some(lit) => lit.base10_parse::<u64>()?,
        None => 0u64,
    };

    // ─── Codegen ─────────────────────────────────────────────────────────────
    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let state_contract_impl = match &state_max_cardinality_lit {
        Some(lit) => {
            let state_max_cardinality_value = lit.base10_parse::<u64>()?;
            // The derived `state_extent` reports a fixed cardinality of 1, so a
            // declared bound above 1 would be vacuous (always 1 <= n). Reject it:
            // multi-key projections must hand-implement `EventSourced` with a real
            // `state_extent()` so the P2 bound check stays meaningful.
            if state_max_cardinality_value != 1 {
                return Err(syn::Error::new_spanned(
                    lit,
                    "#[derive(EventSourced)] supports only single-aggregate state (n = 1); \
                     implement EventSourced by hand with a real `state_extent()` for multi-key state",
                ));
            }
            quote! {
                const STATE_CONTRACT: ::batpak::event::ProjectionStateContract =
                    ::batpak::event::ProjectionStateContract::Bounded {
                        key_space: ::core::concat!(
                            ::core::module_path!(),
                            "::",
                            ::core::stringify!(#ident)
                        ),
                        max_cardinality: #state_max_cardinality_value,
                        retention_policy: "derive-event-sourced-state-object",
                        compaction_policy: "projection-cache-overwrite",
                        checkpoint_policy: "projection-cache",
                    };

                fn state_extent(&self) -> ::batpak::event::StateExtent {
                    let _ = self;
                    ::batpak::event::StateExtent::cardinality(
                        1,
                        ::batpak::event::StateExtentCost::ConstantTime,
                    )
                }
            }
        }
        None => quote! {},
    };

    // Build apply_event dispatch arms — one per event-binding. Handlers
    // take `&T` so users can read fields without being forced to consume
    // the payload (the clippy::needless_pass_by_value would otherwise fire
    // on every handler that does not move-use its argument).
    let arms: Vec<proc_macro2::TokenStream> = bindings
        .iter()
        .map(|b| {
            let event_ty = &b.event;
            let handler_fn = &b.handler;
            quote! {
                // route_typed → Ok(Some(p)): matched kind, decode ok → call handler
                // route_typed → Ok(None):    wrong kind (normal filter) → fall through
                // route_typed → Err(e):      matched kind but decode failed → hard
                //                             correctness signal, panic with source
                match ::batpak::event::DecodeTyped::route_typed::<#event_ty>(event) {
                    ::core::result::Result::Ok(::core::option::Option::Some(__p)) => {
                        self.#handler_fn(&__p);
                        return;
                    }
                    ::core::result::Result::Ok(::core::option::Option::None) => {}
                    ::core::result::Result::Err(__e) => {
                        ::core::panic!(
                            "EventSourced: decode failed for matched kind {}: {}",
                            ::core::stringify!(#event_ty),
                            __e
                        );
                    }
                }
            }
        })
        .collect();

    // relevant_event_kinds: compile-time const array from the event= list.
    let kind_exprs: Vec<proc_macro2::TokenStream> = bindings
        .iter()
        .map(|b| {
            let event_ty = &b.event;
            quote! {
                <#event_ty as ::batpak::event::EventPayload>::KIND
            }
        })
        .collect();
    let kind_count = bindings.len();

    // Handler-signature pins live inside a generic impl so they can reference
    // `Self`-with-type-params. Module-scope `const _: fn(...)` items can't
    // reintroduce generics; this pattern does. When the user's
    // `fn on_x(&mut self, &T)` has the wrong parameter types, rustc spans the
    // error at the generated fn-pointer coercion rather than inside an opaque
    // dispatch arm.
    let handler_checks: Vec<proc_macro2::TokenStream> = bindings
        .iter()
        .map(|b| {
            let event_ty = &b.event;
            let handler_fn = &b.handler;
            quote! {
                let _: fn(&mut Self, &#event_ty) = Self::#handler_fn;
            }
        })
        .collect();

    // C5: pin the `input = T` attribute's type to `ProjectionInput` at
    // derive-expansion site. A non-`ProjectionInput` `input` errors here with
    // the attribute's path visible in the trace, rather than bubbling up from
    // inside generated trait-impl machinery.
    let input_assertion = {
        quote! {
            const _: fn() = || {
                fn __batpak_assert_projection_input<T: ::batpak::event::ProjectionInput>() {}
                __batpak_assert_projection_input::<#input_path>();
            };
        }
    };

    Ok(quote! {
        #input_assertion

        impl #impl_generics ::batpak::event::EventSourced for #ident #ty_generics #where_clause {
            type Input = #input_path;

            #state_contract_impl

            fn from_events(
                events: &[::batpak::event::ProjectionEvent<Self>],
            ) -> ::core::option::Option<Self> {
                if events.is_empty() {
                    return ::core::option::Option::None;
                }
                let mut state: Self = ::core::default::Default::default();
                for __ev in events {
                    state.apply_event(__ev);
                }
                ::core::option::Option::Some(state)
            }

            fn apply_event(&mut self, event: &::batpak::event::ProjectionEvent<Self>) {
                #(#handler_checks)*
                // Each arm keeps wrong-kind filtering (Ok(None)) separate from
                // matched-kind decode failure (Err). A fall-through past all
                // arms means "kind outside relevant_event_kinds()" — normal
                // skip, not an error.
                #(#arms)*
                // Fall-through: unrelated kind. No-op.
                let _ = event;
            }

            fn relevant_event_kinds() -> &'static [::batpak::event::EventKind] {
                static KINDS: [::batpak::event::EventKind; #kind_count] = [
                    #(#kind_exprs),*
                ];
                &KINDS
            }

            fn schema_version() -> u64 {
                // `cache_version` is the projection-cache invalidation key.
                // Unrelated to payload wire `type_id` — they live in different
                // layers (ADR-0010 vs this derive).
                #cache_version_value
            }
        }
    })
}

trait ToTokenStreamString {
    fn to_token_stream_string(&self) -> String;
}

impl ToTokenStreamString for Path {
    fn to_token_stream_string(&self) -> String {
        quote!(#self).to_string()
    }
}

/// Enforce that an `event = <Path>` attribute value is a single-segment,
/// unqualified type name (no `crate::`, no `my_mod::`, no leading `::`).
///
/// The derive deduplicates event bindings by stringifying the path — if
/// multi-segment paths were allowed, `Foo` and `crate::Foo` could alias the
/// same type but compare unequal, producing undetected duplicates. Requiring a
/// single-segment name lets stringified comparison act as a semantic compare
/// without running full path resolution. Users who need a type from another
/// module bring it into scope with `use`.
fn require_single_segment_event_path(path: &Path) -> syn::Result<()> {
    if path.leading_colon.is_some() || path.segments.len() != 1 {
        return Err(syn::Error::new_spanned(
            path,
            "event type must be named by its in-scope single-segment name — use a `use` import if the type is in another module",
        ));
    }
    Ok(())
}

// ─── MultiEventReactor derive expansion ──────────────────────────────────────

fn expand_multi_event_reactor(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    ensure_named_field_struct(input, "MultiEventReactor")?;

    let batpak_attrs: Vec<&Attribute> = input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("batpak"))
        .collect();

    if batpak_attrs.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[derive(MultiEventReactor)] requires `#[batpak(input = <Lane>)]` plus at least one `#[batpak(event = <Payload>, handler = <fn>)]` attribute",
        ));
    }

    let mut input_path: Option<Path> = None;
    let mut error_path: Option<Path> = None;
    let mut bindings: Vec<EventBinding> = Vec::new();
    let mut seen_events: HashSet<String> = HashSet::new();

    for attr in &batpak_attrs {
        match classify_batpak_attr(attr)? {
            BatpakAttrKind::Config {
                input: attr_input,
                cache_version,
                state_max_cardinality,
                error: attr_error,
            } => {
                if let Some(lit) = cache_version {
                    return Err(syn::Error::new(
                        lit.span(),
                        "`cache_version` is not valid on `#[derive(MultiEventReactor)]` — \
                         `cache_version` is a projection-cache key, not a reactor setting",
                    ));
                }
                if let Some(lit) = state_max_cardinality {
                    return Err(syn::Error::new(
                        lit.span(),
                        "`state_max_cardinality` is not valid on `#[derive(MultiEventReactor)]` — \
                         state cardinality is a projection contract, not a reactor setting",
                    ));
                }
                if let Some(path) = attr_input {
                    if input_path.is_some() {
                        return Err(syn::Error::new(
                            path.span(),
                            "duplicate `input =` across `#[batpak(...)]` config attributes — `input` must appear exactly once",
                        ));
                    }
                    input_path = Some(path);
                }
                if let Some(path) = attr_error {
                    if error_path.is_some() {
                        return Err(syn::Error::new(
                            path.span(),
                            "duplicate `error =` across `#[batpak(...)]` config attributes — `error` must appear exactly once",
                        ));
                    }
                    error_path = Some(path);
                }
            }
            BatpakAttrKind::Event(binding) => {
                collect_unique_event_binding(&mut bindings, &mut seen_events, binding, "reactor")?;
            }
        }
    }

    let input_path = input_path.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "#[derive(MultiEventReactor)] requires `#[batpak(input = <Lane>)]` — e.g. `input = JsonValueInput` or `input = RawMsgpackInput`",
        )
    })?;
    let error_path = error_path.ok_or_else(|| {
        syn::Error::new(
            input.ident.span(),
            "#[derive(MultiEventReactor)] requires `#[batpak(error = <ErrorType>)]` — the shared error type all handlers return",
        )
    })?;

    if bindings.is_empty() {
        return Err(syn::Error::new(
            input.ident.span(),
            "#[derive(MultiEventReactor)] requires at least one `#[batpak(event = <Payload>, handler = <fn>)]`",
        ));
    }

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let kind_exprs: Vec<proc_macro2::TokenStream> = bindings
        .iter()
        .map(|b| {
            let event_ty = &b.event;
            quote! {
                <#event_ty as ::batpak::event::EventPayload>::KIND
            }
        })
        .collect();
    let kind_count = bindings.len();

    // Generate dispatch arms. Each arm uses DecodeTyped::route_typed on
    // the inner Event to decode matched kinds to the bound type, then
    // builds &StoredEvent<T> (carrying the source coordinate) for the
    // handler. Wrong-kind events fall through and return Ok(());
    // matched-kind decode failure returns MultiDispatchError::Decode.
    let arms: Vec<proc_macro2::TokenStream> = bindings
        .iter()
        .map(|b| {
            let event_ty = &b.event;
            let handler_fn = &b.handler;
            quote! {
                match ::batpak::event::DecodeTyped::route_typed::<#event_ty>(&event.event) {
                    ::core::result::Result::Ok(::core::option::Option::Some(__p)) => {
                        let __typed_event = ::batpak::event::StoredEvent {
                            coordinate: event.coordinate.clone(),
                            event: ::batpak::event::Event {
                                header: event.event.header.clone(),
                                payload: __p,
                                hash_chain: event.event.hash_chain.clone(),
                            },
                        };
                        return self
                            .#handler_fn(&__typed_event, out, at_least_once)
                            .map_err(::batpak::event::MultiDispatchError::User);
                    }
                    ::core::result::Result::Ok(::core::option::Option::None) => {}
                    ::core::result::Result::Err(__e) => {
                        return ::core::result::Result::Err(
                            ::batpak::event::MultiDispatchError::Decode(__e)
                        );
                    }
                }
            }
        })
        .collect();

    // Handler-signature pins live inside a generic impl so they can reference
    // `Self`-with-type-params. Module-scope `const _: fn(...)` items can't
    // reintroduce generics; this pattern does. Mismatched handler signatures
    // surface as span-pointed errors at the user's handler, not inside the
    // dispatch body.
    let handler_checks: Vec<proc_macro2::TokenStream> = bindings
        .iter()
        .map(|b| {
            let event_ty = &b.event;
            let handler_fn = &b.handler;
            quote! {
                let _: fn(
                    &mut Self,
                    &::batpak::event::StoredEvent<#event_ty>,
                    &mut ::batpak::store::ReactionBatch,
                    ::core::option::Option<&::batpak::store::AtLeastOnce>,
                ) -> ::core::result::Result<(), #error_path> = Self::#handler_fn;
            }
        })
        .collect();

    // C5: pin attribute types at expansion site. A non-`ProjectionInput`
    // `input` or a non-`std::error::Error + Send + Sync + 'static` `error`
    // errors here with the attribute's path visible in the trace, rather than
    // bubbling up from inside generated trait-impl machinery.
    let attr_assertions = {
        quote! {
            const _: fn() = || {
                fn __batpak_assert_projection_input<T: ::batpak::event::ProjectionInput>() {}
                __batpak_assert_projection_input::<#input_path>();
            };
            const _: fn() = || {
                fn __batpak_assert_error<
                    T: ::core::marker::Send
                        + ::core::marker::Sync
                        + 'static
                        + ::std::error::Error,
                >() {}
                __batpak_assert_error::<#error_path>();
            };
        }
    };

    Ok(quote! {
        #attr_assertions

        impl #impl_generics ::batpak::event::MultiReactive<#input_path>
        for #ident #ty_generics #where_clause
        {
            type Error = #error_path;

            fn relevant_event_kinds() -> &'static [::batpak::event::EventKind] {
                static KINDS: [::batpak::event::EventKind; #kind_count] = [
                    #(#kind_exprs),*
                ];
                &KINDS
            }

            fn dispatch(
                &mut self,
                event: &::batpak::event::StoredEvent<
                    <#input_path as ::batpak::event::ProjectionInput>::Payload,
                >,
                out: &mut ::batpak::store::ReactionBatch,
                at_least_once: ::core::option::Option<&::batpak::store::AtLeastOnce>,
            ) -> ::core::result::Result<(), ::batpak::event::MultiDispatchError<Self::Error>> {
                #(#handler_checks)*
                #(#arms)*
                // Wrong kind / no binding matched — silent filter.
                ::core::result::Result::Ok(())
            }
        }
    })
}
