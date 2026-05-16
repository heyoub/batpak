//! Procedural macros for syncbat operation kits.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, Error, Expr, ExprLit, FnArg, Ident, ItemFn, Lit, MetaNameValue, Result,
    Token,
};

/// Generate a syncbat operation descriptor and optional registration function.
#[proc_macro_attribute]
pub fn operation(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as OperationArgs);
    let function = parse_macro_input!(item as ItemFn);

    match expand_operation(args, function) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

struct OperationArgs {
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
    name: Lit,
    effect: Ident,
    input_schema: Lit,
    output_schema: Lit,
    receipt_kind: Lit,
    title: Option<Lit>,
}

fn expand_operation(args: OperationArgs, function: ItemFn) -> Result<proc_macro2::TokenStream> {
    validate_function(&function)?;
    let parsed = parse_args(args)?;

    let fn_name = &function.sig.ident;
    let descriptor = &parsed.descriptor;
    let name = &parsed.name;
    let effect = &parsed.effect;
    let input_schema = &parsed.input_schema;
    let output_schema = &parsed.output_schema;
    let receipt_kind = &parsed.receipt_kind;
    let descriptor_expr = if let Some(title) = &parsed.title {
        quote! {
            ::syncbat::OperationDescriptor::new(
                #name,
                ::syncbat::EffectClass::#effect,
                #input_schema,
                #output_schema,
                #receipt_kind,
            ).with_title(#title)
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

    let register_fn = parsed.register.map(|register| {
        quote! {
            pub fn #register(
                builder: &mut ::syncbat::CoreBuilder,
            ) -> ::std::result::Result<&mut ::syncbat::CoreBuilder, ::syncbat::BuildError> {
                builder.register(#descriptor, #fn_name)
            }
        }
    });

    Ok(quote! {
        #function

        const #descriptor: ::syncbat::OperationDescriptor = #descriptor_expr;

        const _: fn(&[u8], &mut ::syncbat::Cx<'_>) -> ::syncbat::HandlerResult = #fn_name;

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
            "#[syncbat::operation] handlers must accept `&[u8]` and `&mut syncbat::Cx<'_>`",
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
    let mut descriptor = None;
    let mut register = None;
    let mut name = None;
    let mut effect = None;
    let mut input_schema = None;
    let mut output_schema = None;
    let mut receipt_kind = None;
    let mut title = None;

    for pair in args.pairs {
        let key = pair
            .path
            .get_ident()
            .ok_or_else(|| Error::new(pair.path.span(), "expected operation attribute key"))?
            .to_string();
        match key.as_str() {
            "descriptor" => set_ident(&mut descriptor, "descriptor", &pair)?,
            "register" => set_ident(&mut register, "register", &pair)?,
            "name" => set_string(&mut name, "name", &pair)?,
            "effect" => set_effect(&mut effect, &pair)?,
            "input_schema" => set_string(&mut input_schema, "input_schema", &pair)?,
            "output_schema" => set_string(&mut output_schema, "output_schema", &pair)?,
            "receipt_kind" => set_string(&mut receipt_kind, "receipt_kind", &pair)?,
            "title" => set_string(&mut title, "title", &pair)?,
            other => {
                return Err(Error::new(
                    pair.path.span(),
                    format!("unknown key `{other}` in #[syncbat::operation]"),
                ));
            }
        }
    }

    Ok(ParsedOperationArgs {
        descriptor: required(descriptor, "descriptor")?,
        register,
        name: required(name, "name")?,
        effect: required(effect, "effect")?,
        input_schema: required(input_schema, "input_schema")?,
        output_schema: required(output_schema, "output_schema")?,
        receipt_kind: required(receipt_kind, "receipt_kind")?,
        title,
    })
}

fn set_ident(target: &mut Option<Ident>, key: &str, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            format!("duplicate `{key}` key in #[syncbat::operation]"),
        ));
    }
    match &pair.value {
        Expr::Path(path) if path.path.segments.len() == 1 && path.path.get_ident().is_some() => {
            *target = path.path.get_ident().cloned();
            Ok(())
        }
        _ => Err(Error::new(
            pair.value.span(),
            format!("`{key}` must be a Rust identifier"),
        )),
    }
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

fn set_effect(target: &mut Option<Ident>, pair: &MetaNameValue) -> Result<()> {
    if target.is_some() {
        return Err(Error::new(
            pair.path.span(),
            "duplicate `effect` key in #[syncbat::operation]",
        ));
    }
    match &pair.value {
        Expr::Path(path) if path.path.segments.len() == 1 => {
            let Some(ident) = path.path.get_ident() else {
                return Err(Error::new(
                    path.path.span(),
                    "`effect` must be a syncbat EffectClass variant identifier",
                ));
            };
            match ident.to_string().as_str() {
                "Inspect" | "Compute" | "Persist" | "Emit" | "Control" => {
                    *target = Some(ident.clone());
                    Ok(())
                }
                other => Err(Error::new(
                    ident.span(),
                    format!("unsupported effect `{other}` in #[syncbat::operation]"),
                )),
            }
        }
        _ => Err(Error::new(
            pair.value.span(),
            "`effect` must be a syncbat EffectClass variant identifier",
        )),
    }
}

fn string_lit(expr: &Expr) -> Option<Lit> {
    match expr {
        Expr::Lit(ExprLit {
            lit: lit @ Lit::Str(_),
            ..
        }) => Some(lit.clone()),
        _ => None,
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
