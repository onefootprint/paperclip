//! Convenience macros for the [actix-web](https://github.com/paperclip-rs/paperclip/tree/master/plugins/actix-web)
//! OpenAPI plugin (exposed by paperclip with `actix` feature).

use heck::*;
use http::StatusCode;
use lazy_static::lazy_static;
use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use strum_macros::EnumString;
use syn::{
    parse_macro_input,
    punctuated::{Pair, Punctuated},
    spanned::Spanned,
    Attribute, Data, DataEnum, DeriveInput, Field, Fields, FieldsNamed, FieldsUnnamed, FnArg,
    Generics, Ident, ItemFn, Lit, Meta, MetaList, MetaNameValue, NestedMeta, Path, PathArguments,
    ReturnType, Token, TraitBound, Type, TypeTraitObject,
};

use proc_macro2::TokenStream as TokenStream2;
use std::{collections::HashMap, str::FromStr};

const SCHEMA_MACRO_ATTR: &str = "openapi";

lazy_static! {
    static ref EMPTY_SCHEMA_HELP: String = format!(
        "you can mark the struct with #[{}(empty)] to ignore this warning.",
        SCHEMA_MACRO_ATTR
    );
}

/// Actual parser and emitter for `api_v2_operation` macro.
pub fn emit_v2_operation(attrs: TokenStream, input: TokenStream) -> TokenStream {
    let default_span = proc_macro2::Span::call_site();
    let mut item_ast: ItemFn = match syn::parse(input) {
        Ok(s) => s,
        Err(e) => {
            emit_error!(e.span().unwrap(), "operation must be a function.");
            return quote!().into();
        }
    };

    // Unit struct
    let s_name = format!("paperclip_{}", item_ast.sig.ident);
    let unit_struct = Ident::new(&s_name, default_span);
    let generics = &item_ast.sig.generics;
    let mut generics_call = quote!();
    let mut struct_definition = quote!(
        #[allow(non_camel_case_types, missing_docs)]
        struct #unit_struct;
    );
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    if !generics.params.is_empty() {
        let turbofish = ty_generics.as_turbofish();
        let generics_params = extract_generics_params(&item_ast);
        generics_call = quote!(#turbofish { p: std::marker::PhantomData });
        struct_definition = quote!(struct #unit_struct #ty_generics { p: std::marker::PhantomData<(#generics_params)> } )
    }

    // Get rid of async prefix. In the end, we'll have them all as `impl Future` thingies.
    if item_ast.sig.asyncness.is_some() {
        item_ast.sig.asyncness = None;
    }

    let mut wrapper = quote!(paperclip::actix::ResponseWrapper<actix_web::HttpResponse, #unit_struct #ty_generics>);
    let mut is_impl_trait = false;
    let mut is_responder = false;
    match &mut item_ast.sig.output {
        rt @ ReturnType::Default => {
            // Not particularly useful, but let's deal with it anyway
            *rt = ReturnType::Type(
                Token![->](default_span),
                Box::new(syn::parse2(wrapper.clone()).expect("parsing empty type")),
            );
        }
        ReturnType::Type(_, ty) => {
            let t = quote!(#ty).to_string();
            if let Type::ImplTrait(_) = &**ty {
                is_impl_trait = true;
            }

            if t == "impl Responder" {
                // `impl Responder` is a special case because we have to add another wrapper.
                // FIXME: Better way to deal with this?
                is_responder = true;
                *ty = Box::new(
                    syn::parse2(quote!(
                        impl std::future::Future<Output=paperclip::actix::ResponderWrapper<#ty>>
                    ))
                    .expect("parsing impl trait"),
                );
            } else if !is_impl_trait {
                // Any handler that's not returning an impl trait should return an `impl Future`
                *ty = Box::new(
                    syn::parse2(quote!(impl std::future::Future<Output=#ty>))
                        .expect("parsing impl trait"),
                );
            }

            if let Type::ImplTrait(imp) = &**ty {
                let obj = TypeTraitObject {
                    dyn_token: Some(Token![dyn](default_span)),
                    bounds: imp.bounds.clone(),
                };
                *ty = Box::new(
                    syn::parse2(quote!(#ty + paperclip::v2::schema::Apiv2Operation))
                        .expect("parsing impl trait"),
                );

                if !is_responder {
                    // NOTE: We're only using the box "type" to generate the operation data, we're not boxing
                    // the handlers at runtime.
                    wrapper = quote!(paperclip::actix::ResponseWrapper<Box<#obj + std::marker::Unpin>, #unit_struct #ty_generics>);
                }
            }
        }
    }

    let block = item_ast.block;
    // We need a function because devs should be able to use "return" keyword along the way.
    let wrapped_fn_call = if is_responder {
        quote!(paperclip::util::ready(paperclip::actix::ResponderWrapper((move || #block)())))
    } else if is_impl_trait {
        quote!((move || #block)())
    } else {
        quote!((move || async move #block)())
    };

    item_ast.block = Box::new(
        syn::parse2(quote!(
            {
                let f = #wrapped_fn_call;
                paperclip::actix::ResponseWrapper {
                    responder: f,
                    operations: #unit_struct #generics_call,
                }
            }
        ))
        .expect("parsing wrapped block"),
    );

    // Initialize operation parameters from macro attributes
    let (mut op_params, mut op_values) = parse_operation_attrs(attrs);

    if op_params.iter().any(|i| *i == "skip") {
        return quote!(
            #[allow(non_camel_case_types, missing_docs)]
            #struct_definition

            #item_ast

            impl #impl_generics paperclip::v2::schema::Apiv2Operation for #unit_struct #ty_generics #where_clause {
                fn operation() -> paperclip::v2::models::DefaultOperationRaw {
                    Default::default()
                }

                #[allow(unused_mut)]
                fn security_definitions() -> std::collections::BTreeMap<String, paperclip::v2::models::SecurityScheme> {
                    Default::default()
                }

                fn definitions() -> std::collections::BTreeMap<String, paperclip::v2::models::DefaultSchemaRaw> {
                    Default::default()
                }

                fn is_visible() -> bool {
                    false
                }
            }
        ).into();
    }

    // Optionally extract summary and description from doc comments
    if !op_params.iter().any(|i| *i == "summary") {
        let (summary, description) = extract_fn_documentation(&item_ast);
        if let Some(summary) = summary {
            op_params.push(Ident::new("summary", item_ast.span()));
            op_values.push(summary)
        }
        if let Some(description) = description {
            op_params.push(Ident::new("description", item_ast.span()));
            op_values.push(description)
        }
    }

    if op_params.iter().any(|i| *i == "deprecated") || extract_deprecated(&item_ast.attrs) {
        op_params.push(Ident::new("deprecated", item_ast.span()));
        op_values.push(quote!(true))
    }

    let modifiers = extract_fn_arguments_types(&item_ast);

    let operation_modifier = if is_responder {
        quote! { paperclip::actix::ResponderWrapper::<actix_web::HttpResponse> }
    } else {
        quote! { <<#wrapper as std::future::Future>::Output> }
    };

    quote!(
        #struct_definition

        #item_ast

        impl #impl_generics paperclip::v2::schema::Apiv2Operation for #unit_struct #ty_generics #where_clause {
            fn operation() -> paperclip::v2::models::DefaultOperationRaw {
                use paperclip::actix::OperationModifier;
                let mut op = paperclip::v2::models::DefaultOperationRaw {
                    #(
                        #op_params: #op_values,
                    )*
                    .. Default::default()
                };
                #(
                    <#modifiers>::update_parameter(&mut op);
                    <#modifiers>::update_security(&mut op);
                )*
                #operation_modifier::update_response(&mut op);
                op
            }

            #[allow(unused_mut)]
            fn security_definitions() -> std::collections::BTreeMap<String, paperclip::v2::models::SecurityScheme> {
                use paperclip::actix::OperationModifier;
                let mut map = Default::default();
                #(
                    <#modifiers>::update_security_definitions(&mut map);
                )*
                map
            }

            fn definitions() -> std::collections::BTreeMap<String, paperclip::v2::models::DefaultSchemaRaw> {
                use paperclip::actix::OperationModifier;
                let mut map = std::collections::BTreeMap::new();
                #(
                    <#modifiers>::update_definitions(&mut map);
                )*
                #operation_modifier::update_definitions(&mut map);
                map
            }
        }
    )
        .into()
}

/// Extract punctuated generic parameters from fn definition
fn extract_generics_params(item_ast: &ItemFn) -> Punctuated<Ident, syn::token::Comma> {
    item_ast
        .sig
        .generics
        .params
        .pairs()
        .filter_map(|pair| match pair {
            Pair::Punctuated(syn::GenericParam::Type(gen), punct) => {
                Some(Pair::new(gen.ident.clone(), Some(*punct)))
            }
            Pair::End(syn::GenericParam::Type(gen)) => Some(Pair::new(gen.ident.clone(), None)),
            _ => None,
        })
        .collect()
}

/// Extract function arguments
fn extract_fn_arguments_types(item_ast: &ItemFn) -> Vec<Type> {
    item_ast
        .sig
        .inputs
        .iter()
        .filter_map(|inp| match inp {
            FnArg::Receiver(_) => None,
            FnArg::Typed(ref t) => Some(*t.ty.clone()),
        })
        .collect()
}

/// Parse macro attrs, matching to Operation fields
/// Returning operation attribute identifier and value initialization arrays
/// Note: Array likes initialized from string "val1, val2, val3", where "val1"
/// would parse into destination item
fn parse_operation_attrs(attrs: TokenStream) -> (Vec<Ident>, Vec<proc_macro2::TokenStream>) {
    let attrs = crate::parse_input_attrs(attrs);
    let mut params = Vec::new();
    let mut values = Vec::new();
    for attr in attrs.0 {
        match &attr {
            NestedMeta::Meta(Meta::Path(attr_path)) => {
                if let Some(attr_) = attr_path.get_ident() {
                    if *attr_ == "skip" || *attr_ == "deprecated" {
                        params.push(attr_.clone());
                    } else {
                        emit_error!(attr_.span(), "Not supported bare attribute {:?}", attr_)
                    }
                }
            }
            NestedMeta::Meta(Meta::NameValue(MetaNameValue { path, lit, .. })) => {
                if let Some(ident) = path.get_ident() {
                    match ident.to_string().as_str() {
                        "summary" | "description" | "operation_id" => {
                            if let Lit::Str(val) = lit {
                                params.push(ident.clone());
                                values.push(quote!(Some(# val.to_string())));
                            } else {
                                emit_error!(lit.span(), "Expected string literal: {:?}", lit)
                            }
                        }
                        "consumes" | "produces" => {
                            if let Lit::Str(mimes) = lit {
                                let mut mime_types = Vec::new();
                                for val in mimes.value().split(',') {
                                    let val = val.trim();
                                    if let Err(err) = val.parse::<mime::Mime>() {
                                        emit_error!(
                                            lit.span(),
                                            "Value {} does not parse as mime type: {}",
                                            val,
                                            err
                                        );
                                    } else {
                                        mime_types.push(quote!(paperclip::v2::models::MediaRange( # val.parse().unwrap())));
                                    }
                                }
                                if !mime_types.is_empty() {
                                    params.push(ident.clone());
                                    values.push(quote!({
                                    let mut tmp = std::collections::BTreeSet::new();
                                    # (
                                    tmp.insert(# mime_types);
                                    ) *
                                    Some(tmp)
                                    }));
                                }
                            } else {
                                emit_error!(
                                    lit.span(),
                                    "Expected comma separated values in string literal: {:?}",
                                    lit
                                )
                            }
                        }
                        x => emit_error!(ident.span(), "Unknown attribute {}", x),
                    }
                } else {
                    emit_error!(
                        path.span(),
                        "Expected single identifier, got path {:?}",
                        path
                    )
                }
            }
            NestedMeta::Meta(Meta::List(MetaList { path, nested, .. })) => {
                if let Some(ident) = path.get_ident() {
                    match ident.to_string().as_str() {
                        "tags" => {
                            let mut tags = Vec::new();
                            for meta in nested.pairs().map(|pair| pair.into_value()) {
                                if let NestedMeta::Meta(Meta::Path(Path { segments, .. })) = meta {
                                    tags.push(segments[0].ident.to_string());
                                } else if let NestedMeta::Lit(Lit::Str(lit)) = meta {
                                    tags.push(lit.value());
                                } else {
                                    emit_error!(
                                        meta.span(),
                                        "Expected comma separated list of tags idents: {:?}",
                                        meta
                                    )
                                }
                            }
                            if !tags.is_empty() {
                                params.push(ident.clone());
                                values.push(quote!(vec![ #( #tags.to_string() ),* ]));
                            }
                        }
                        x => emit_error!(ident.span(), "Unknown list ident {}", x),
                    }
                }
            }
            _ => {
                emit_error!(attr.span(), "Not supported attribute type {:?}", attr)
            }
        }
    }
    (params, values)
}

/// Extracts summary from top line doc comment and description from the rest
fn extract_fn_documentation(
    item_ast: &ItemFn,
) -> (
    Option<proc_macro2::TokenStream>,
    Option<proc_macro2::TokenStream>,
) {
    let docs = extract_documentation(&item_ast.attrs);
    let lines = docs.lines();
    let mut before_empty = true;
    let (summary, description): (Vec<_>, Vec<_>) = lines.partition(|line| {
        if line.trim().is_empty() {
            before_empty = false
        };
        before_empty
    });
    let none_if_empty = |text: &str| {
        if text.is_empty() {
            None
        } else {
            Some(quote!(Some(#text.to_string())))
        }
    };
    let summary = none_if_empty(summary.into_iter().collect::<String>().trim());
    let description = none_if_empty(description.join("\n").trim());
    (summary, description)
}

/// Actual parser and emitter for `api_v2_errors` macro.
pub fn emit_v2_errors(attrs: TokenStream, input: TokenStream) -> TokenStream {
    let item_ast = match crate::expect_struct_or_enum(input) {
        Ok(i) => i,
        Err(ts) => return ts,
    };

    let name = &item_ast.ident;
    let attrs = crate::parse_input_attrs(attrs);
    let generics = item_ast.generics.clone();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut default_schema: Option<syn::Ident> = None;
    // Convert macro attributes to tuples in form of (u16, &str, &Option<syn::Ident>)
    let error_codes = attrs
        .0
        .iter()
        // Pair code attrs with description attrs; save attr itself to properly span error messages at later stage
        .fold(Vec::new(), |mut list: Vec<(Option<u16>, Option<String>, Option<syn::Ident>, _)>, attr| {
            let span = attr.span().unwrap();
            match attr {
                // Read named attribute.
                NestedMeta::Meta(Meta::NameValue(name_value)) => {
                    let attr_name = name_value.path.get_ident().map(|ident| ident.to_string());
                    let attr_value = &name_value.lit;
                    match (attr_name.as_deref(), attr_value) {
                        // "code" attribute adds new element to list
                        (Some("code"), Lit::Int(attr_value)) => {
                            let status_code = attr_value.base10_parse::<u16>()
                                .map_err(|_| emit_error!(span, "Invalid u16 in code argument")).ok();
                            list.push((status_code, None, None, attr));
                        }
                        // "description" attribute updates last element in list
                        (Some("description"), Lit::Str(attr_value)) =>
                            if let Some(last_value) = list.last_mut() {
                                if last_value.1.is_some() {
                                    emit_warning!(span, "This attribute overwrites previous description");
                                }
                                last_value.1 = Some(attr_value.value());
                            } else {
                                emit_error!(span, "Attribute 'description' can be only placed after prior 'code' argument");
                            },
                        // "schema" attribute updates last element in list
                        (Some("schema"), Lit::Str(attr_value)) =>
                            if let Some(last_value) = list.last_mut() {
                                if last_value.2.is_some() {
                                    emit_warning!(span, "This attribute overwrites previous schema");
                                }
                                match attr_value.parse() {
                                    Ok(value) => last_value.2 = Some(value),
                                    Err(error) => emit_error!(span, "Error parsing schema: {}", error),
                                }
                            } else {
                                emit_error!(span, "Attribute 'schema' can be only placed after prior 'code' argument");
                            },
                        (Some("default_schema"), Lit::Str(attr_value)) =>
                            match attr_value.parse() {
                                Ok(value) => default_schema = Some(value),
                                Err(error) => emit_error!(span, "Error parsing default_schema: {}", error),
                            },
                        _ => emit_error!(span, "Invalid macro attribute. Should be plain u16, 'code = u16', 'description = str', 'schema = str' or 'default_schema = str'")
                    }
                }
                // Read plain status code as attribute.
                NestedMeta::Lit(Lit::Int(attr_value)) => {
                    let status_code = attr_value.base10_parse::<u16>()
                        .map_err(|_| emit_error!(span, "Invalid u16 in code argument")).ok();
                    list.push((status_code, None, None, attr));
                }
                _ => emit_error!(span, "This macro supports only named attributes - 'code' (u16), 'description' (str), 'schema' (str) or 'default_schema' (str)")
            }

            list
        })
        .iter()
        // Map code-message pairs into bits of code, filter empty codes out
        .filter_map(|quad| {
            let (code, description, schema) = match quad {
                (Some(code), Some(description), schema, _) => {
                    (code, description.to_owned(), schema.to_owned())
                }
                (Some(code), None, schema, attr) => {
                    let span = attr.span().unwrap();
                    let description = StatusCode::from_u16(*code)
                        .map_err(|_| {
                            emit_warning!(span, format!("Invalid status code {}", code));
                            String::new()
                        })
                        .map(|s| s.canonical_reason()
                            .map(str::to_string)
                            .unwrap_or_else(|| {
                                emit_warning!(span, format!("Status code {} doesn't have a canonical name", code));
                                String::new()
                            })
                        )
                        .unwrap_or_else(|_| String::new());
                    (code, description, schema.to_owned())
                }
                (None, _, _, _) => return None,
            };
            Some((*code, description, schema))
        })
        .collect::<Vec<(u16, String, Option<syn::Ident>)>>();

    let error_definitions = error_codes.iter().fold(
        if default_schema.is_none() {
            TokenStream2::new()
        } else {
            quote! {
                #default_schema::update_definitions(map);
            }
        },
        |mut stream, (_, _, schema)| {
            if let Some(schema) = schema {
                let tokens = quote! {
                    #schema::update_definitions(map);
                };
                stream.extend(tokens);
            }
            stream
        },
    );

    let update_definitions = quote! {
        fn update_definitions(map: &mut std::collections::BTreeMap<String, paperclip::v2::models::DefaultSchemaRaw>) {
            use paperclip::actix::OperationModifier;
            #error_definitions
        }
    };

    // for compatibility with previous error trait
    let error_map = error_codes.iter().fold(
        proc_macro2::TokenStream::new(),
        |mut stream, (code, description, _)| {
            let token = quote! {
                (#code, #description),
            };
            stream.extend(token);
            stream
        },
    );

    let update_error_helper = quote! {
        fn update_error_definitions(code: &u16, description: &str, schema: &Option<&str>, op: &mut paperclip::v2::models::DefaultOperationRaw) {
            if let Some(schema) = &schema {
                op.responses.insert(code.to_string(), paperclip::v2::models::Either::Right(paperclip::v2::models::Response {
                    description: Some(description.to_string()),
                    schema: Some(paperclip::v2::models::DefaultSchemaRaw {
                        name: Some(schema.to_string()),
                        reference: Some(format!("#/definitions/{}", schema)),
                        .. Default::default()
                    }),
                    ..Default::default()
                }));
            } else {
                op.responses.insert(code.to_string(), paperclip::v2::models::Either::Right(paperclip::v2::models::DefaultResponseRaw {
                    description: Some(description.to_string()),
                    ..Default::default()
                }));
            }
        }
    };
    let default_schema = default_schema.map(|i| i.to_string());
    let update_errors = error_codes.iter().fold(
        update_error_helper,
        |mut stream, (code, description, schema)| {
            let tokens = if let Some(schema) = schema {
                let schema = schema.to_string();
                quote! {
                    update_error_definitions(&#code, #description, &Some(#schema), op);
                }
            } else if let Some(scheme) = &default_schema {
                quote! {
                    update_error_definitions(&#code, #description, &Some(#scheme), op);
                }
            } else {
                quote! {
                    update_error_definitions(&#code, #description, &None, op);
                }
            };
            stream.extend(tokens);
            stream
        },
    );

    let gen = quote! {
        #item_ast

        impl #impl_generics paperclip::v2::schema::Apiv2Errors for #name #ty_generics #where_clause {
            const ERROR_MAP: &'static [(u16, &'static str)] = &[
                #error_map
            ];
            fn update_error_definitions(op: &mut paperclip::v2::models::DefaultOperationRaw) {
                #update_errors
            }
            #update_definitions
        }
    };

    gen.into()
}

/// Actual parser and emitter for `emit_v2_errors_overlay` macro.
pub fn emit_v2_errors_overlay(attrs: TokenStream, input: TokenStream) -> TokenStream {
    let item_ast = match crate::expect_struct_or_enum(input) {
        Ok(i) => i,
        Err(ts) => return ts,
    };

    let name = &item_ast.ident;
    let inner = match &item_ast.data {
        Data::Struct(s) => if s.fields.len() == 1 {
            match &s.fields {
                Fields::Unnamed(s) => s.unnamed.first().map(|s| match &s.ty {
                    Type::Path(s) => s.path.segments.first().map(|f| &f.ident),
                    _ => None,
                }),
                _ => None,
            }
        } else {
            None
        }
        .flatten()
        .unwrap_or_else(|| {
            abort!(
                s.fields.span(),
                "This macro supports only unnamed structs with 1 element"
            )
        }),
        _ => {
            abort!(item_ast.span(), "This macro supports only unnamed structs");
        }
    };

    let attrs = crate::parse_input_attrs(attrs);
    let generics = item_ast.generics.clone();
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // Convert macro attributes to vector of u16
    let error_codes = attrs
        .0
        .iter()
        // Pair code attrs with description attrs; save attr itself to properly span error messages at later stage
        .fold(Vec::new(), |mut list: Vec<u16>, attr| {
            let span = attr.span().unwrap();
            match attr {
                // Read plain status code as attribute.
                NestedMeta::Lit(Lit::Int(attr_value)) => {
                    let status_code = attr_value
                        .base10_parse::<u16>()
                        .map_err(|_| emit_error!(span, "Invalid u16 in code argument"))
                        .unwrap();
                    list.push(status_code);
                }
                _ => emit_error!(
                    span,
                    "This macro supports only named attributes - 'code' (u16)"
                ),
            }

            list
        });
    let filter_error_codes = error_codes
        .iter()
        .fold(TokenStream2::new(), |mut stream, code| {
            let status_code = &code.to_string();
            let tokens = quote! {
                op.responses.remove(#status_code);
            };
            stream.extend(tokens);
            stream
        });

    let gen = quote! {
        #item_ast

        impl std::fmt::Display for #name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl actix_web::error::ResponseError for #name {
            fn status_code(&self) -> actix_web::http::StatusCode {
                self.0.status_code()
            }
            fn error_response(&self) -> actix_web::HttpResponse {
                self.0.error_response()
            }
        }

        impl #impl_generics paperclip::v2::schema::Apiv2Errors for #name #ty_generics #where_clause {
            const ERROR_MAP: &'static [(u16, &'static str)] = &[];
            fn update_definitions(map: &mut std::collections::BTreeMap<String, paperclip::v2::models::DefaultSchemaRaw>) {
                #inner::update_definitions(map);
            }
            fn update_error_definitions(op: &mut paperclip::v2::models::DefaultOperationRaw) {
                #inner::update_error_definitions(op);
                #filter_error_codes
            }
        }
    };

    gen.into()
}

fn extract_rename(attrs: &[Attribute]) -> Option<String> {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        if let NestedMeta::Meta(Meta::NameValue(nv)) = attr {
            if nv.path.is_ident("rename") {
                if let Lit::Str(s) = nv.lit {
                    return Some(s.value());
                } else {
                    emit_error!(
                        nv.lit.span().unwrap(),
                        format!(
                            "`#[{}(rename = \"...\")]` expects a string argument",
                            SCHEMA_MACRO_ATTR
                        ),
                    );
                }
            }
        }
    }

    None
}

fn extract_openapi_gated(attrs: &[Attribute]) -> Option<syn::Expr> {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        let nv = match attr {
            NestedMeta::Meta(Meta::NameValue(nv)) if nv.path.is_ident("gated") => nv,
            _ => continue,
        };
        let Lit::Str(ref s) = nv.lit else {
            emit_error!(
                nv.lit.span().unwrap(),
                format!(
                    "`#[{}(gated = \"...\")]` expects a string argument",
                    SCHEMA_MACRO_ATTR
                ),
            );
            return None;
        };

        let Ok(ts) = proc_macro::TokenStream::from_str(&s.value()) else {
            emit_error!(
                nv.lit.span().unwrap(),
                format!(
                    "`#[{}(gated = \"...\")]` expects a string argument parseable as a tokenstream",
                    SCHEMA_MACRO_ATTR
                ),
            );
            return None;
        };

        let Ok(expr) = syn::parse::<syn::Expr>(ts) else {
            emit_error!(
                nv.lit.span().unwrap(),
                format!(
                    "`#[{}(gated = \"...\")]` expects a string argument parseable as an Expr",
                    SCHEMA_MACRO_ATTR
                ),
            );
            return None;
        };

        return Some(expr);
    }

    None
}

fn extract_example(attrs: &[Attribute]) -> Option<String> {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        if let NestedMeta::Meta(Meta::NameValue(nv)) = attr {
            if nv.path.is_ident("example") {
                if let Lit::Str(s) = nv.lit {
                    return Some(s.value());
                } else {
                    emit_error!(
                        nv.lit.span().unwrap(),
                        format!(
                            "`#[{}(example = \"...\")]` expects a string argument",
                            SCHEMA_MACRO_ATTR
                        ),
                    );
                }
            }
        }
    }

    None
}

fn extract_is_inline(attrs: &[Attribute]) -> bool {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        if let NestedMeta::Meta(Meta::Path(attr_path)) = attr {
            if let Some(attr) = attr_path.get_ident() {
                if *attr == "inline" {
                    return true;
                }
            }
        }
    }

    false
}

fn extract_is_collapsed(attrs: &[Attribute]) -> bool {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        if let NestedMeta::Meta(Meta::Path(attr_path)) = attr {
            if let Some(attr) = attr_path.get_ident() {
                if *attr == "collapsed" {
                    return true;
                }
            }
        }
    }

    false
}

fn extract_priority(attrs: &[Attribute]) -> Option<u32> {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        if let NestedMeta::Meta(Meta::NameValue(nv)) = attr {
            if nv.path.is_ident("priority") {
                if let Lit::Int(s) = nv.lit {
                    return Some(s.base10_parse::<u32>().unwrap());
                } else {
                    emit_error!(
                        nv.lit.span().unwrap(),
                        format!(
                            "`#[{}(priority = \"...\")]` expects a u32 argument",
                            SCHEMA_MACRO_ATTR
                        ),
                    );
                }
            }
        }
    }

    None
}

fn extract_serialize_as(attrs: &[Attribute]) -> Option<Type> {
    let attrs = extract_openapi_attrs(attrs);
    for attr in attrs.flat_map(|attr| attr.into_iter()) {
        let nv = match attr {
            NestedMeta::Meta(Meta::NameValue(nv)) if nv.path.is_ident("serialize_as") => nv,
            _ => continue,
        };
        let Lit::Str(ref s) = nv.lit else {
            emit_error!(
                nv.lit.span().unwrap(),
                format!(
                    "`#[{}(serialize_as = \"...\")]` expects a string argument",
                    SCHEMA_MACRO_ATTR
                ),
            );
            return None;
        };

        let Ok(ts) = proc_macro::TokenStream::from_str(&s.value()) else {
            emit_error!(
                nv.lit.span().unwrap(),
                format!(
                    "`#[{}(serialize_as = \"...\")]` expects a string argument parseable as a tokenstream",
                    SCHEMA_MACRO_ATTR
                ),
            );
            return None;
        };
        let Ok(ty) = syn::parse::<Type>(ts) else {
            emit_error!(
                nv.lit.span().unwrap(),
                format!(
                    "`#[{}(serialize_as = \"...\")]` expects a string argument parseable as a Type",
                    SCHEMA_MACRO_ATTR
                ),
            );
            return None;
        };

        return Some(ty);
    }

    None
}

/// Actual parser and emitter for `api_v2_schema` macro.
pub fn emit_v2_definition(input: TokenStream, for_response: bool) -> TokenStream {
    let item_ast = match crate::expect_struct_or_enum(input) {
        Ok(i) => i,
        Err(ts) => return ts,
    };

    if let Some(empty) = check_empty_schema(&item_ast) {
        return empty;
    }

    let docs = extract_documentation(&item_ast.attrs);
    let docs = docs.trim();

    let example = if let Some(example) = extract_example(&item_ast.attrs) {
        // allow to parse escaped json string or single str value
        quote!(
            serde_json::from_str::<serde_json::Value>(#example).ok().or_else(|| Some(#example.into()))
        )
    } else {
        quote!(None)
    };

    let props = SerdeProps::from_item_attrs(&item_ast.attrs);

    let name = &item_ast.ident;

    // Add `Apiv2Schema` bound for impl if the type is generic.
    let mut generics = item_ast.generics.clone();
    let bound = syn::parse2::<TraitBound>(quote!(paperclip::v2::schema::Apiv2Schema))
        .expect("expected to parse trait bound");
    generics.type_params_mut().for_each(|param| {
        param.bounds.push(bound.clone().into());
    });

    let operation_modifier_impl = if for_response {
        add_response_operation_modifier_impl(name, &generics)
    } else {
        add_optional_impl(name, &generics)
    };
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    // FIXME: Use attr path segments to find flattening, skipping, etc.
    let mut props_gen = quote! {};

    match &item_ast.data {
        Data::Struct(ref s) => {
            props_gen.extend(quote!(
                schema.data_type = Some(DataType::Object);
            ));
            match &s.fields {
                Fields::Named(ref f) => {
                    handle_field_struct(f, &item_ast.attrs, &props, &mut props_gen)
                }
                Fields::Unnamed(ref f) => {
                    handle_unnamed_field_struct(f, &item_ast.attrs, &mut props_gen)
                }
                Fields::Unit => {
                    emit_warning!(
                        s.struct_token.span().unwrap(),
                        "unit structs do not have any fields and hence will have empty schema.";
                        help = "{}", &*EMPTY_SCHEMA_HELP;
                    );
                }
            }
        }
        Data::Enum(ref e) => handle_enum(e, &item_ast.attrs, &props, &mut props_gen),
        Data::Union(ref u) => emit_error!(
            u.union_token.span().unwrap(),
            "unions are unsupported for deriving schema"
        ),
    };

    let base_name = extract_rename(&item_ast.attrs).unwrap_or_else(|| name.to_string());
    let type_params: Vec<&Ident> = generics.type_params().map(|p| &p.ident).collect();
    let schema_name = if type_params.is_empty() {
        quote! { #base_name }
    } else {
        let type_names = quote! {
            [#(#type_params::name()),*]
                .iter()
                .filter_map(|n| n.to_owned())
                .collect::<Vec<String>>()
                .join(", ")
        };
        quote! { format!("{}<{}>", #base_name, #type_names) }
    };
    let props_gen_empty = props_gen.is_empty();

    let is_inline = extract_is_inline(&item_ast.attrs);
    let open_api_name = if is_inline {
        // If the struct is marked as "inline", we omit generating a name. This makes it so we cannot refer
        // to the schema by name, so we will always inline its definition
        quote!{ None }
    } else {
        #[cfg(not(feature = "path-in-definition"))]
        quote! { Some(#schema_name.into()) }
        #[cfg(feature = "path-in-definition")]
        quote! { Some(Self::__paperclip_schema_name()) }
    };

    let default_schema_raw_def = quote! {
        let mut schema = DefaultSchemaRaw {
            name: #open_api_name,
            example: #example,
            ..Default::default()
        };
    };

    #[cfg(not(feature = "path-in-definition"))]
    let paperclip_schema_name_def = quote!();

    #[cfg(feature = "path-in-definition")]
    let paperclip_schema_name_def = quote! {
        fn __paperclip_schema_name() -> String {
            // The module path itself, e.g cratename::module
            let full_module_path = std::module_path!().to_string();
            // We're not interested in the crate name, nor do we want :: as a seperator
            let trimmed_module_path = full_module_path.split("::")
                .enumerate()
                .filter(|(index, _)| *index != 0) // Skip the first element, i.e the crate name
                .map(|(_, component)| component)
                .collect::<Vec<_>>()
                .join("_");
            format!("{}_{}", trimmed_module_path, #schema_name)
        }
    };

    #[cfg(not(feature = "path-in-definition"))]
    let const_name_def = quote! {
        fn name() -> Option<String> {
            #open_api_name
        }
    };

    // TODO: not sure why this doesn't define the name() fn
    #[cfg(feature = "path-in-definition")]
    let const_name_def = quote!();

    let gen = quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            #paperclip_schema_name_def
        }

        impl #impl_generics paperclip::v2::schema::Apiv2Schema for #name #ty_generics #where_clause {
            #const_name_def
            fn description() -> &'static str {
                #docs
            }

            fn raw_schema() -> paperclip::v2::models::DefaultSchemaRaw {
                use paperclip::v2::models::{DataType, DataTypeFormat, DefaultSchemaRaw};
                use paperclip::v2::schema::TypedData;

                #default_schema_raw_def

                #props_gen
                // props_gen may override the schema for unnamed structs with 1 element
                // as it replaces the struct type with inner type.
                // make sure we set the name properly if props_gen is not empty
                if !#props_gen_empty {
                    schema.name = #open_api_name;
                }
                schema
            }
        }

        #operation_modifier_impl
    };

    gen.into()
}

/// Actual parser and emitter for `Apiv2Security` derive macro.
pub fn emit_v2_security(input: TokenStream) -> TokenStream {
    let item_ast = match crate::expect_struct_or_enum(input) {
        Ok(i) => i,
        Err(ts) => return ts,
    };

    if let Some(empty) = check_empty_schema(&item_ast) {
        return empty;
    }

    let name = &item_ast.ident;
    // Add `Apiv2Schema` bound for impl if the type is generic.
    let mut generics = item_ast.generics.clone();
    let bound = syn::parse2::<TraitBound>(quote!(paperclip::v2::schema::Apiv2Schema))
        .expect("expected to parse trait bound");
    generics.type_params_mut().for_each(|param| {
        param.bounds.push(bound.clone().into());
    });

    let opt_impl = add_optional_impl(name, &generics);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut security_attrs = HashMap::new();
    let mut scopes = Vec::new();

    let valid_attrs = vec![
        "alias",
        "description",
        "name",
        "in",
        "flow",
        "auth_url",
        "token_url",
        "parent",
    ];
    let invalid_attr_msg = format!("Invalid macro attribute. Should be bare security type [\"apiKey\", \"oauth2\"] or named attribute {:?}", valid_attrs);

    // Read security params from openapi attr.
    for nested in extract_openapi_attrs(&item_ast.attrs) {
        for nested_attr in nested {
            let span = nested_attr.span().unwrap();
            match &nested_attr {
                // Read bare attribute.
                NestedMeta::Meta(Meta::Path(attr_path)) => {
                    if let Some(type_) = attr_path.get_ident() {
                        if security_attrs
                            .insert("type".to_string(), type_.to_string())
                            .is_some()
                        {
                            emit_warning!(span, "Auth type defined multiple times.");
                        }
                    }
                }
                // Read named attribute.
                NestedMeta::Meta(Meta::NameValue(name_value)) => {
                    let attr_name = name_value.path.get_ident().map(|id| id.to_string());
                    let attr_value = &name_value.lit;

                    if let Some(attr_name) = attr_name {
                        if valid_attrs.contains(&attr_name.as_str()) {
                            if let Lit::Str(attr_value) = attr_value {
                                if security_attrs
                                    .insert(attr_name.clone(), attr_value.value())
                                    .is_some()
                                {
                                    emit_warning!(
                                        span,
                                        "Attribute {} defined multiple times.",
                                        attr_name
                                    );
                                }
                            } else {
                                emit_warning!(
                                    span,
                                    "Invalid value for named attribute: {}",
                                    attr_name
                                );
                            }
                        } else {
                            emit_warning!(span, invalid_attr_msg);
                        }
                    } else {
                        emit_error!(span, invalid_attr_msg);
                    }
                }
                // Read scopes attribute
                NestedMeta::Meta(Meta::List(list_attr)) => {
                    match list_attr
                        .path
                        .get_ident()
                        .map(|id| id.to_string())
                        .as_deref()
                    {
                        Some("scopes") => {
                            for nested in &list_attr.nested {
                                match nested {
                                    NestedMeta::Lit(Lit::Str(value)) => {
                                        scopes.push(value.value().to_string())
                                    }
                                    _ => emit_error!(
                                        nested.span().unwrap(),
                                        "Invalid list attribute value"
                                    ),
                                }
                            }
                        }
                        Some(path) => emit_error!(span, "Invalid list attribute: {}", path),
                        _ => emit_error!(span, "Invalid list attribute"),
                    }
                }
                _ => {
                    emit_error!(span, invalid_attr_msg);
                }
            }
        }
    }

    let scopes_stream = scopes
        .iter()
        .fold(proc_macro2::TokenStream::new(), |mut stream, scope| {
            stream.extend(quote! {
                oauth2_scopes.insert(#scope.to_string(), #scope.to_string());
            });
            stream
        });

    let (security_def, security_def_name) = match (
        security_attrs.get("type"),
        security_attrs.get("parent"),
    ) {
        (Some(type_), None) => {
            let alias = security_attrs.get("alias").unwrap_or(type_);
            let quoted_description = quote_option(security_attrs.get("description"));
            let quoted_name = quote_option(security_attrs.get("name"));
            let quoted_in = quote_option(security_attrs.get("in"));
            let quoted_flow = quote_option(security_attrs.get("flow"));
            let quoted_auth_url = quote_option(security_attrs.get("auth_url"));
            let quoted_token_url = quote_option(security_attrs.get("token_url"));

            (
                Some(quote! {
                    Some(paperclip::v2::models::SecurityScheme {
                        type_: #type_.to_string(),
                        name: #quoted_name,
                        in_: #quoted_in,
                        flow: #quoted_flow,
                        auth_url: #quoted_auth_url,
                        token_url: #quoted_token_url,
                        scopes: std::collections::BTreeMap::new(),
                        description: #quoted_description,
                    })
                }),
                Some(quote!(Some(#alias.to_string()))),
            )
        }
        (None, Some(parent)) => {
            let parent_ident = Ident::new(parent, proc_macro2::Span::call_site());
            // Child of security definition (Scopes will be glued to parent definition).
            (
                Some(quote! {
                    let mut oauth2_scopes = std::collections::BTreeMap::new();
                    #scopes_stream
                    let mut scheme = #parent_ident::security_scheme()
                        .expect("empty schema. did you derive `Apiv2Security` for parent struct?");
                    scheme.scopes = oauth2_scopes;
                    Some(scheme)
                }),
                Some(quote!(<#parent_ident as paperclip::v2::schema::Apiv2Schema>::name())),
            )
        }
        (Some(_), Some(_)) => {
            emit_error!(
                item_ast.span().unwrap(),
                "Can't define new security type and use parent attribute together."
            );
            (None, None)
        }
        (None, None) => {
            emit_error!(
                item_ast.span().unwrap(),
                "Invalid attributes. Expected security type or parent defined."
            );
            (None, None)
        }
    };

    let gen = if let (Some(def_block), Some(def_name)) = (security_def, security_def_name) {
        quote! {
            impl #impl_generics paperclip::v2::schema::Apiv2Schema for #name #ty_generics #where_clause {
                fn name() -> Option<String> {
                    #def_name
                }

                fn security_scheme() -> Option<paperclip::v2::models::SecurityScheme> {
                    #def_block
                }
            }

            #opt_impl
        }
    } else {
        quote! {}
    };

    gen.into()
}

/// Actual parser and emitter for `Apiv2Header` derive macro.
pub fn emit_v2_header(input: TokenStream) -> TokenStream {
    let item_ast = match crate::expect_struct_or_enum(input) {
        Ok(i) => i,
        Err(ts) => return ts,
    };

    if let Some(empty) = check_empty_schema(&item_ast) {
        return empty;
    }

    let name = &item_ast.ident;
    // Add `Apiv2Schema` bound for impl if the type is generic.
    let mut generics = item_ast.generics.clone();
    let bound = syn::parse2::<TraitBound>(quote!(paperclip::v2::schema::Apiv2Schema))
        .expect("expected to parse trait bound");
    generics.type_params_mut().for_each(|param| {
        param.bounds.push(bound.clone().into());
    });

    let opt_impl = add_optional_impl(name, &generics);
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let mut header_definitions = vec![];

    let valid_attrs = vec!["description", "name", "format"];
    let invalid_attr_msg = format!(
        "Invalid macro attribute. Should be named attribute {:?}",
        valid_attrs
    );

    fn quote_format(format: &str) -> proc_macro2::TokenStream {
        match format {
            "int32" => quote! { Some(paperclip::v2::models::DataTypeFormat::Int32) },
            "int64" => quote! { Some(paperclip::v2::models::DataTypeFormat::Int64) },
            "float" => quote! { Some(paperclip::v2::models::DataTypeFormat::Float) },
            "double" => quote! { Some(paperclip::v2::models::DataTypeFormat::Double) },
            "byte" => quote! { Some(paperclip::v2::models::DataTypeFormat::Byte) },
            "binary" => quote! { Some(paperclip::v2::models::DataTypeFormat::Binary) },
            "date" => quote! { Some(paperclip::v2::models::DataTypeFormat::Date) },
            "datetime" | "date-time" => {
                quote! { Some(paperclip::v2::models::DataTypeFormat::DateTime) }
            }
            "password" => quote! { Some(paperclip::v2::models::DataTypeFormat::Password) },
            "url" => quote! { Some(paperclip::v2::models::DataTypeFormat::Url) },
            "uuid" => quote! { Some(paperclip::v2::models::DataTypeFormat::Uuid) },
            "ip" => quote! { Some(paperclip::v2::models::DataTypeFormat::Ip) },
            "ipv4" => quote! { Some(paperclip::v2::models::DataTypeFormat::IpV4) },
            "ipv6" => quote! { Some(paperclip::v2::models::DataTypeFormat::IpV6) },
            "other" => quote! { Some(paperclip::v2::models::DataTypeFormat::Other) },
            v => {
                emit_error!(
                    format.span().unwrap(),
                    format!("Invalid format attribute value. Got {}", v)
                );
                quote! { None }
            }
        }
    }

    let struct_ast = match &item_ast.data {
        Data::Struct(struct_ast) => struct_ast,
        Data::Enum(_) | Data::Union(_) => {
            emit_error!(
                item_ast.span(),
                "Invalid data type. Apiv2Header should be defined on a struct"
            );
            return quote!().into();
        }
    };

    if extract_openapi_attrs(&item_ast.attrs)
        .peekable()
        .peek()
        .is_some()
    {
        emit_error!(
            item_ast.span(),
            "Invalid openapi attribute. openapi attribute should be defined at struct fields level"
        );
        return quote!().into();
    }

    for field in &struct_ast.fields {
        let mut parameter_attrs = HashMap::new();
        let field_name = &field.ident;
        let docs = extract_documentation(&field.attrs);
        let docs = docs.trim();

        // Read header params from openapi attr.
        for nested in extract_openapi_attrs(&field.attrs) {
            for nested_attr in nested {
                let span = nested_attr.span().unwrap();
                match &nested_attr {
                    // Read bare attribute (support for skip attribute)
                    NestedMeta::Meta(Meta::Path(attr_path)) => {
                        if let Some(attr) = attr_path.get_ident() {
                            if *attr == "skip" {
                                parameter_attrs.insert("skip".to_owned(), "".to_owned());
                            }
                        }
                    }
                    // Read named attribute.
                    NestedMeta::Meta(Meta::NameValue(name_value)) => {
                        let attr_name = name_value.path.get_ident().map(|id| id.to_string());
                        let attr_value = &name_value.lit;

                        if let Some(attr_name) = attr_name {
                            if valid_attrs.contains(&attr_name.as_str()) {
                                if let Lit::Str(attr_value) = attr_value {
                                    if parameter_attrs
                                        .insert(attr_name.clone(), attr_value.value())
                                        .is_some()
                                    {
                                        emit_warning!(
                                            span,
                                            "Attribute {} defined multiple times.",
                                            attr_name
                                        );
                                    }
                                } else {
                                    emit_warning!(
                                        span,
                                        "Invalid value for named attribute: {}",
                                        attr_name
                                    );
                                }
                            } else {
                                emit_warning!(span, invalid_attr_msg);
                            }
                        } else {
                            emit_error!(span, invalid_attr_msg);
                        }
                    }
                    _ => {
                        emit_error!(span, invalid_attr_msg);
                    }
                }
            }
        }

        if parameter_attrs.contains_key("skip") {
            continue;
        }

        let docs = (!docs.is_empty()).then(|| docs.to_owned());
        let quoted_description = quote_option(parameter_attrs.get("description").or(docs.as_ref()));
        let name_string = field_name.as_ref().map(|name| name.to_string());
        let quoted_name = if let Some(name) = parameter_attrs.get("name").or(name_string.as_ref()) {
            name
        } else {
            emit_error!(
                field.span(),
                "Missing header name. Either add a name using the openapi attribute or use named struct parameter"
            );
            return quote!().into();
        };

        let (quoted_type, quoted_format) = if let Some(ty_ref) = get_field_type(field) {
            (
                quote! { {
                    use paperclip::v2::schema::TypedData;
                    Some(#ty_ref::data_type())
                } },
                quote! { {
                    use paperclip::v2::schema::TypedData;
                    #ty_ref::format()
                } },
            )
        } else {
            (quote! { None }, quote! { None })
        };

        let (quoted_type, quoted_format) = if let Some(format) = parameter_attrs.get("format") {
            let quoted_format = quote_format(format);
            let quoted_type = quote! { #quoted_format.map(|format| format.into()) };
            (quoted_type, quoted_format)
        } else {
            (quoted_type, quoted_format)
        };

        let def_block = quote! {
            paperclip::v2::models::Parameter::<paperclip::v2::models::DefaultSchemaRaw> {
                name: #quoted_name.to_owned(),
                in_: paperclip::v2::models::ParameterIn::Header,
                description: #quoted_description,
                data_type: #quoted_type,
                format: #quoted_format,
                required: Self::required(),
                ..Default::default()
            }
        };

        header_definitions.push(def_block);
    }

    let gen = quote! {
        impl #impl_generics paperclip::v2::schema::Apiv2Schema for #name #ty_generics #where_clause {
            fn header_parameter_schema() -> Vec<paperclip::v2::models::Parameter<paperclip::v2::models::DefaultSchemaRaw>> {
                vec![
                    #(#header_definitions),*
                ]
            }
        }

        #opt_impl
    };

    gen.into()
}

fn quote_option(value: Option<&String>) -> proc_macro2::TokenStream {
    if let Some(value) = value {
        quote! { Some(#value.to_string()) }
    } else {
        quote! { None }
    }
}

#[cfg(feature = "nightly")]
fn add_optional_impl(_: &Ident, _: &Generics) -> proc_macro2::TokenStream {
    // Empty impl for "nightly" feature because specialization helps us there.
    quote!()
}

#[cfg(not(feature = "nightly"))]
fn add_optional_impl(name: &Ident, generics: &Generics) -> proc_macro2::TokenStream {
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    quote! {
        impl #impl_generics paperclip::actix::OperationModifier for #name #ty_generics #where_clause {}
    }
}

fn add_response_operation_modifier_impl(
    name: &Ident,
    generics: &Generics,
) -> proc_macro2::TokenStream {
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    // Copied from core/src/v2/actix.rs
    quote! {
        impl #impl_generics paperclip::actix::OperationModifier for #name #ty_generics #where_clause {
            fn update_parameter(op: &mut paperclip::v2::models::DefaultOperationRaw) {
                let description = <Self as paperclip::v2::schema::Apiv2Schema>::description().to_owned();
                op.parameters.push(paperclip::v2::models::Either::Right(paperclip::v2::models::Parameter {
                    description: (!description.is_empty()).then_some(description),
                    in_: paperclip::v2::models::ParameterIn::Body,
                    name: "body".into(),
                    required: true,
                    schema: Some({
                        let mut def = <Self as paperclip::v2::schema::Apiv2Schema>::schema_with_ref();
                        def.retain_ref();
                        def
                    }),
                    ..Default::default()
                }));
            }

            fn update_response(op: &mut paperclip::v2::models::DefaultOperationRaw) {
                op.responses.insert(
                    "200".into(),
                    paperclip::v2::models::Either::Right(paperclip::v2::models::Response {
                        // TODO: Support configuring other 2xx codes using macro attribute.
                        description: Some("OK".into()),
                        schema: Some({
                            let mut def = <Self as paperclip::v2::schema::Apiv2Schema>::schema_with_ref();
                            def.retain_ref();
                            def
                        }),
                        ..Default::default()
                    }),
                );
            }
        }
    }
}

fn get_field_type(field: &Field) -> Option<proc_macro2::TokenStream> {
    if let Some(serialize_as_ty) = extract_serialize_as(&field.attrs) {
        return Some(address_type_for_fn_call(&serialize_as_ty));
    }
    match field.ty {
        Type::Path(_) | Type::Reference(_) => Some(address_type_for_fn_call(&field.ty)),
        _ => {
            emit_warning!(
                field.ty.span().unwrap(),
                "unsupported field type will be ignored."
            );
            None
        }
    }
}

/// Given the attributes of a struct, generates a TokenStream that adds `description`, `extensions`, and `example` to a schema named `s`
fn extract_metadata(attrs: &[Attribute]) -> TokenStream2 {
    let docs = extract_documentation(attrs);
    let docs = docs.trim();

    let docs = if !docs.is_empty() {
        quote!(s.description = Some(#docs.to_string());)
    } else {
        quote!()
    };

    // Serialize a custom `x_fp_preview_gate` field that communicates which preview API is needed in order to render this field.
    let preview_gate = if let Some(gated) = extract_openapi_gated(&attrs) {
        quote!(
            s.extensions.insert("x_fp_preview_gate".to_string(), serde_json::Value::String(#gated.to_string()));
        )
    } else {
        quote!()
    };

    let collapsed = if extract_is_collapsed(&attrs) {
        quote!(s.extensions.insert("x_fp_collapsed".to_string(), serde_json::Value::Bool(true));)
    } else {
        quote!()
    };

    let priority = if let Some(priority) = extract_priority(&attrs) {
        quote!(s.extensions.insert("x_fp_priority".to_string(), serde_json::Value::Number(priority.into()));)
    } else {
        quote!()
    };

    let example = if let Some(example) = extract_example(&attrs) {
        // allow to parse escaped json string or single str value
        quote!(
            s.example = serde_json::from_str::<serde_json::Value>(#example).ok().or_else(|| Some(#example.into()));
        )
    } else {
        quote!()
    };

    quote!(
        #docs
        #preview_gate
        #collapsed
        #priority
        #example
    )
}

/// Adds metadata from the provided `attributes` to the schema defined by `schema_ref`.
fn add_metadata_to_schema(schema_ref: TokenStream2, metadata: TokenStream2) -> TokenStream2 {
    if metadata.is_empty() {
        return quote!(
            let s = #schema_ref;
        )
    }
    // Add the provided metadata to the schema_ref, either inline or as an allOf
    quote!(
        let mut s = #schema_ref;
        if s.name.is_some() {
            // Schema is a complex type referred to by a reference.
            // Use allOf to allow overriding metadata fields
            s = DefaultSchemaRaw::default();
            s.all_of.push({
                let original_s = #schema_ref;
                Box::new(original_s)
            });
            s.all_of.push({
                // Add metadata as a separate schema
                let mut s = DefaultSchemaRaw::default();
                s.data_type = #schema_ref.data_type;
                #metadata
                Box::new(s)
            });
        } else {
            // The main schema is a simple type, can just add additional fields inline
            #metadata
        }
    )
}


/// Generates code for a tuple struct with fields.
fn handle_unnamed_field_struct(
    fields: &FieldsUnnamed,
    struct_attr: &[Attribute],
    props_gen: &mut proc_macro2::TokenStream,
) {
    if fields.unnamed.len() == 1 {
        // Tuple with only one element
        let field = fields.unnamed.iter().next().unwrap();

        if let Some(ty_ref) = get_field_type(field) {
            // Generally extract metadata from the parent struct's attributes
            let metadata = extract_metadata(struct_attr);
            if SerdeSkip::exists(&field.attrs) {
                props_gen.extend(quote!({
                    let mut s: DefaultSchemaRaw = Default::default();
                    #metadata
                    schema = s;
                }));
            } else {
                let inline = OpenApiInline::exists(&field.attrs);
                let schema_ref = if inline {
                    quote!(#ty_ref::raw_schema())
                } else {
                    quote!(#ty_ref::schema_with_ref())
                };

                let s_definition = add_metadata_to_schema(schema_ref, metadata);
                props_gen.extend(quote!({
                    #s_definition
                    schema = s;
                }));
            }
        }
    } else {
        // Tuple with multiple elements
        for (inner_field_id, field) in fields.unnamed.iter().enumerate() {
            if SerdeSkip::exists(&field.attrs) {
                continue;
            }

            let ty_ref = get_field_type(field);

            let docs = extract_documentation(&field.attrs);
            let docs = docs.trim();

            let inline = OpenApiInline::exists(&field.attrs);
            let schema_ref = if inline {
                quote!(#ty_ref::raw_schema())
            } else {
                quote!(#ty_ref::schema_with_ref())
            };

            if !SerdeFlatten::exists(&field.attrs) {
                let override_required = OpenApiRequired::exists(&field.attrs);
                let override_optional = OpenApiOptional::exists(&field.attrs);
                // this is really not what we'd want to do because that's not how the
                // deserialized struct will be like, ideally we want an actual tuple
                // this type should therefore not be used for anything else than `Path`
                props_gen.extend(
                    quote!({
                        let mut s = #schema_ref;
                        if !#docs.is_empty() {
                            s.description = Some(#docs.to_string());
                        }
                        schema.properties.insert(#inner_field_id.to_string(), s.into());
                        if (#ty_ref::required() || #override_required) && !#override_optional {
                            schema.required.insert(#inner_field_id.to_string());
                        }
                    })
                );
            } else {
                props_gen.extend(flattened_schema(schema_ref));
            }
        }
    }
}

/// Given a `schema_ref` of a schema who should be serde flattened into a variable of name `schema`, returns
/// the tokenstream that redefines the `schema` variable as the flattened schema that joins `schema_ref` and `schema`.
fn flattened_schema(schema_ref: TokenStream2) -> TokenStream2 {
    quote!({
        let mut s = DefaultSchemaRaw::default();
        s.all_of.push({
            Box::new(schema)
        });
        s.all_of.push({
            Box::new(#schema_ref)
        });
        schema = s;
    })
}

/// Returns the nested meta for all `#[openapi(...)]`` attributes.
/// For example, returns `skip` for #[openapi(skip)] and `serialize_as = Foo` for `#[openapi(serialize_as = Foo)]`
fn extract_openapi_attrs(
    field_attrs: &'_ [Attribute],
) -> impl Iterator<Item = Punctuated<syn::NestedMeta, syn::token::Comma>> + '_ {
    field_attrs.iter().filter_map(|a| match a.parse_meta() {
        Ok(Meta::List(list)) if list.path.is_ident(SCHEMA_MACRO_ATTR) => Some(list.nested),
        _ => None,
    })
}

fn extract_deprecated(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| match a.parse_meta() {
        Ok(Meta::Path(mp)) if mp.is_ident("deprecated") => true,
        Ok(Meta::List(mml)) => mml
            .path
            .segments
            .into_iter()
            .any(|p| p.ident == "deprecated"),
        _ => false,
    })
}

/// Checks for `api_v2_empty` attributes and removes them.
fn extract_documentation(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter_map(|a| match a.parse_meta() {
            Ok(Meta::NameValue(mnv)) if mnv.path.is_ident("doc") => match &mnv.lit {
                Lit::Str(s) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<String>>()
        .join("\n")
}

/// Checks if an empty schema has been requested and generate if needed.
fn check_empty_schema(item_ast: &DeriveInput) -> Option<TokenStream> {
    let needs_empty_schema = extract_openapi_attrs(&item_ast.attrs).any(|nested| {
        nested.len() == 1
            && match &nested[0] {
                NestedMeta::Meta(Meta::Path(path)) => path.is_ident("empty"),
                _ => false,
            }
    });

    if needs_empty_schema {
        let name = &item_ast.ident;
        let generics = item_ast.generics.clone();
        let opt_impl = add_optional_impl(name, &generics);
        let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
        return Some(quote!(
            impl #impl_generics paperclip::v2::schema::Apiv2Schema for #name #ty_generics #where_clause {}

            #opt_impl
        ).into());
    }

    None
}

/// Generates code for a struct with fields.
fn handle_field_struct(
    fields: &FieldsNamed,
    struct_attr: &[Attribute],
    serde: &SerdeProps,
    props_gen: &mut proc_macro2::TokenStream,
) {
    let docs = extract_documentation(struct_attr);
    let docs = docs.trim();

    props_gen.extend(quote!({
        if !#docs.is_empty() {
            schema.description = Some(#docs.to_string());
        }
    }));
    for field in &fields.named {
        let mut field_name = field
            .ident
            .as_ref()
            .expect("missing field name?")
            .to_string();

        //Strip r# prefix if any
        field_name = field_name
            .strip_prefix("r#")
            .map(|n| n.to_string())
            .unwrap_or(field_name);
        if SerdeSkip::exists(&field.attrs) {
            continue;
        }

        if let Some(renamed) = SerdeRename::from_field_attrs(&field.attrs) {
            field_name = renamed;
        } else if let Some(prop) = serde.rename {
            field_name = prop.rename(&field_name);
        }

        let ty_ref = get_field_type(field);
        let inline = OpenApiInline::exists(&field.attrs);
        let schema_ref = if inline {
            quote!(#ty_ref::raw_schema())
        } else {
            quote!(#ty_ref::schema_with_ref())
        };

        if !SerdeFlatten::exists(&field.attrs) {
            let override_required = OpenApiRequired::exists(&field.attrs);
            let override_optional = OpenApiOptional::exists(&field.attrs);
            let metadata = extract_metadata(&field.attrs);
            let s_definition = add_metadata_to_schema(schema_ref, metadata);

            props_gen.extend(
                quote!({
                    #s_definition
                    schema.properties.insert(#field_name.into(), s.into());
                    if (#ty_ref::required() || #override_required) && !#override_optional {
                        schema.required.insert(#field_name.into());
                    }
                })
            );
        } else {
            props_gen.extend(flattened_schema(schema_ref));
        }
    }
}

/// Generates code for an enum (if supported).
fn handle_enum(
    e: &DataEnum,
    attrs: &[Attribute],
    serde: &SerdeProps,
    props_gen: &mut proc_macro2::TokenStream,
) {
    // set whether constants are inline strings
    let simple_constants = serde.enum_tag_type == SerdeEnumTagType::External
        || serde.enum_tag_type == SerdeEnumTagType::Untagged;

    // check if all variants are simple constants and can use `enum`
    // otherwise we'll make use of `any_of`
    let variants = e.variants.iter().filter(|v| !SerdeSkip::exists(&v.attrs)).collect::<Vec<_>>();
    let only_simple_constants = simple_constants
        && !variants
            .iter()
            .any(|variant| variant.fields != Fields::Unit);
    if only_simple_constants {
        // we'll use the enum syntax later on and can declare this to be of type string
        props_gen.extend(quote!(
            schema.data_type = Some(DataType::String);
        ));
    }

    let doc = extract_documentation(attrs);
    let doc = doc.trim();
    if !doc.is_empty() {
        props_gen.extend(quote!(
            schema.description = Some(#doc.into());
        ));
    }
    props_gen.extend(quote!(
        let enum_name = schema.name.clone().unwrap_or_default();
    ));

    for var in &variants {
        if SerdeSkip::exists(&var.attrs) {
            continue;
        }

        let original_name = var.ident.to_string();
        let name = if let Some(renamed) = SerdeRename::from_field_attrs(&var.attrs) {
            renamed
        } else if let Some(prop) = serde.rename {
            prop.rename(&original_name)
        } else {
            original_name.clone()
        };
        props_gen.extend(quote!(
            let enum_variant_name = format!("{}{}", enum_name, #original_name);
        ));

        if only_simple_constants {
            props_gen.extend(quote!(
                schema.enum_.push(serde_json::json!(#name));
            ));
        } else {
            // this will aggregate the construction of the variant schema
            let mut inner_gen = quote!();
            // this indicate if the schema is effectively empty
            let mut inner_gen_empty = false;

            let docs = extract_documentation(&var.attrs);
            let docs = docs.trim();

            match &var.fields {
                Fields::Unit => {
                    // unit constants may be simple constant types
                    if simple_constants {
                        props_gen.extend(quote!(
                            schema.any_of.push(DefaultSchemaRaw {
                                const_: Some(serde_json::json!(#name)),
                                description: if #docs.is_empty() { None } else { Some(#docs.into()) },
                                ..Default::default()
                            }.into());
                        ));
                        continue;
                    }

                    // this is required so there's something to add tags to
                    inner_gen = quote!(
                        let mut schema = DefaultSchemaRaw {
                            data_type: Some(DataType::Object),
                            description: if #docs.is_empty() { None } else { Some(#docs.into()) },
                            ..Default::default()
                        };
                    );
                    inner_gen_empty = true;
                }
                Fields::Named(ref f) => {
                    let metadata = extract_metadata(&var.attrs);
                    inner_gen.extend(quote!(
                        let mut s = DefaultSchemaRaw {
                            data_type: Some(DataType::Object),
                            description: if #docs.is_empty() { None } else { Some(#docs.into()) },
                            ..Default::default()
                        };
                        #metadata
                        let mut schema = s;
                    ));
                    handle_field_struct(f, &[], serde, &mut inner_gen);
                }
                Fields::Unnamed(ref f) => {
                    inner_gen.extend(quote!(
                        let mut schema = DefaultSchemaRaw {
                            data_type: Some(DataType::Object),
                            ..Default::default()
                        };
                    ));
                    // Description will be added in `handle_unnamed_field_struct`
                    handle_unnamed_field_struct(f, &var.attrs, &mut inner_gen);
                }
            }

            match serde.enum_tag_type {
                SerdeEnumTagType::External => {
                    props_gen.extend(quote!(
                        schema.any_of.push({
                            let mut schema = DefaultSchemaRaw {
                                data_type: Some(DataType::Object),
                                ..Default::default()
                            };
                            schema.properties.insert(#name.into(), {
                                #inner_gen
                                schema
                            }.into());
                            schema.required.insert(#name.into());

                            schema
                        }.into());
                    ));
                }
                SerdeEnumTagType::Internal(ref tag) => {
                    if inner_gen_empty {
                        // No need to wrap in an allOf since there is no inner data
                        props_gen.extend(quote!(
                            schema.any_of.push({
                                #inner_gen

                                // Generate a new type whose name is a function of the enum name + variant name
                                schema.reference = Some(format!("#/definitions/{}", enum_variant_name));
                                schema.name = Some(enum_variant_name);

                                let mut tag_schema = DefaultSchemaRaw {
                                    const_: Some(serde_json::json!(#name)),
                                    ..Default::default()
                                };
                                tag_schema.extensions.insert("x_fp_priority".to_string(), serde_json::Value::Number(0.into()));
                                schema.properties.insert(#tag.into(), tag_schema.into());
                                schema.required.insert(#tag.into());
                                schema
                            }.into());
                        ));
                    } else {
                        // The schema of the nested data might be shared. To avoid mutating it, represent the
                        // internal tag as a separate schema in an allOf.
                        props_gen.extend(quote!(
                            schema.any_of.push({
                                #inner_gen
                                // Generate a new type whose name is a function of the enum name + variant name
                                schema.reference = Some(format!("#/definitions/{}", enum_variant_name));
                                schema.name = Some(enum_variant_name);

                                // Add the tag to the existing schema, since we are just definined a new type.
                                // Note: this won't work if the inner schema is an anyOf enum. We'd have to use allOf here.
                                let mut tag_schema = DefaultSchemaRaw {
                                    const_: Some(serde_json::json!(#name)),
                                    ..Default::default()
                                };
                                tag_schema.extensions.insert("x_fp_priority".to_string(), serde_json::Value::Number(0.into()));
                                schema.properties.insert(#tag.into(), tag_schema.into());
                                schema.required.insert(#tag.into());
                                schema
                            }.into());
                        ));
                    }
                }
                SerdeEnumTagType::Adjacent(ref tag, ref content_tag) => {
                    // if the variant schema is empty, we don't need the content tag
                    if inner_gen_empty {
                        props_gen.extend(quote!(
                            schema.any_of.push({
                                let mut schema = DefaultSchemaRaw {
                                    data_type: Some(DataType::Object),
                                    ..Default::default()
                                };
                                // Generate a new type whose name is a function of the enum name + variant name
                                schema.reference = Some(format!("#/definitions/{}", enum_variant_name));
                                schema.name = Some(enum_variant_name);

                                schema.properties.insert(#tag.into(), DefaultSchemaRaw {
                                    const_: Some(serde_json::json!(#name)),
                                    description: if #docs.is_empty() { None } else { Some(#docs.into()) },
                                    ..Default::default()
                                }.into());
                                schema.required.insert(#tag.into());
                                schema
                            }.into());
                        ));
                    } else {
                        props_gen.extend(quote!(
                            schema.any_of.push({
                                let mut wrapper_schema = DefaultSchemaRaw {
                                    data_type: Some(DataType::Object),
                                    ..Default::default()
                                };
                                // Generate a new type whose name is a function of the enum name + variant name
                                wrapper_schema.reference = Some(format!("#/definitions/{}", enum_variant_name));
                                wrapper_schema.name = Some(enum_variant_name);

                                wrapper_schema.properties.insert(#tag.into(), DefaultSchemaRaw {
                                    const_: Some(serde_json::json!(#name)),
                                    description: if #docs.is_empty() { None } else { Some(#docs.into()) },
                                    ..Default::default()
                                }.into());
                                wrapper_schema.properties.insert(#content_tag.into(), {
                                    #inner_gen
                                    schema
                                }.into());
                                wrapper_schema.required.insert(#tag.into());
                                wrapper_schema.required.insert(#content_tag.into());

                                wrapper_schema
                            }.into());
                        ));
                    }
                }
                SerdeEnumTagType::Untagged => {
                    props_gen.extend(quote!(
                        schema.any_of.push({
                            #inner_gen
                            schema
                        }.into());
                    ));
                }
            }
        }
    }
}

/// An associated function of a generic type, say, a vector cannot be called
/// like `Vec::foo` as it doesn't have a default type. We should instead call
/// `Vec::<T>::foo`. Something similar applies to `str`. This function takes
/// care of that special treatment.
fn address_type_for_fn_call(old_ty: &Type) -> proc_macro2::TokenStream {
    if let Type::Reference(_) = old_ty {
        return quote!(<(#old_ty)>);
    }

    let mut ty = old_ty.clone();
    if let Type::Path(ref mut p) = &mut ty {
        p.path.segments.pairs_mut().for_each(|mut pair| {
            let is_empty = pair.value().arguments.is_empty();
            let args = &mut pair.value_mut().arguments;
            match args {
                PathArguments::AngleBracketed(ref mut brack_args) if !is_empty => {
                    brack_args.colon2_token = Some(Token![::](proc_macro2::Span::call_site()));
                }
                _ => (),
            }
        });
    }

    quote!(#ty)
}

/* Serde attributes */

/// Supported renaming options in serde (https://serde.rs/variant-attrs.html).
#[derive(Clone, Copy, Debug, Eq, PartialEq, EnumString)]
enum SerdeRename {
    #[strum(serialize = "lowercase")]
    Lower,
    #[strum(serialize = "UPPERCASE")]
    Upper,
    #[strum(serialize = "PascalCase")]
    Pascal,
    #[strum(serialize = "camelCase")]
    Camel,
    #[strum(serialize = "snake_case")]
    Snake,
    #[strum(serialize = "SCREAMING_SNAKE_CASE")]
    ScreamingSnake,
    #[strum(serialize = "kebab-case")]
    Kebab,
    #[strum(serialize = "SCREAMING-KEBAB-CASE")]
    ScreamingKebab,
}

impl SerdeRename {
    /// Traverses the field attributes and returns the renamed value from the first matching
    /// `#[serde(rename = "...")]` pattern.
    fn from_field_attrs(field_attrs: &[Attribute]) -> Option<String> {
        for meta in field_attrs.iter().filter_map(|a| a.parse_meta().ok()) {
            let inner_meta = match meta {
                Meta::List(ref l)
                    if l.path
                        .segments
                        .last()
                        .map(|p| p.ident == "serde")
                        .unwrap_or(false) =>
                {
                    &l.nested
                }
                _ => continue,
            };

            for meta in inner_meta {
                let rename = match meta {
                    NestedMeta::Meta(Meta::NameValue(ref v))
                        if v.path
                            .segments
                            .last()
                            .map(|p| p.ident == "rename")
                            .unwrap_or(false) =>
                    {
                        &v.lit
                    }
                    _ => continue,
                };

                if let Lit::Str(ref s) = rename {
                    return Some(s.value());
                }
            }
        }

        None
    }

    /// Renames the given value using the current option.
    fn rename(self, name: &str) -> String {
        match self {
            SerdeRename::Lower => name.to_lowercase(),
            SerdeRename::Upper => name.to_uppercase(),
            SerdeRename::Pascal => name.to_pascal_case(),
            SerdeRename::Camel => name.to_lower_camel_case(),
            SerdeRename::Snake => name.to_snake_case(),
            SerdeRename::ScreamingSnake => name.to_shouty_snake_case(),
            SerdeRename::Kebab => name.to_kebab_case(),
            SerdeRename::ScreamingKebab => name.to_shouty_kebab_case(),
        }
    }
}

/// Serde skip (https://serde.rs/variant-attrs.html)
/// Never serialize or deserialize this variant.
/// There are other variants available (skip_serializing,skip_deserializing) though it's not clear
/// how this should be handled since we use the same Schema for Ser/DeSer
struct SerdeSkip;

impl SerdeSkip {
    /// Traverses the field attributes and returns whether the field should be skipped or not
    /// dependent on finding the `#[serde(skip]` attribute.
    fn exists(field_attrs: &[Attribute]) -> bool {
        for meta in field_attrs.iter().filter_map(|a| a.parse_meta().ok()) {
            // Check serde skip
            // And also check for our own #[paperclip(skip)] attribute
            let inner_meta = match meta {
                Meta::List(ref l)
                    if l.path
                        .segments
                        .last()
                        .map(|p| p.ident == "serde" || p.ident == "openapi")
                        .unwrap_or(false) =>
                {
                    &l.nested
                }
                _ => continue,
            };
            for meta in inner_meta {
                if let NestedMeta::Meta(Meta::Path(path)) = meta {
                    if path.segments.iter().any(|s| s.ident == "skip") {
                        return true;
                    }
                }
            }
        }

        false
    }
}

/// Custom attribute that sets this struct as "debug," which is an indicator that paperclip should print some
/// context when compiling the schema.
#[allow(unused)]
struct OpenApiDebug;

impl OpenApiDebug {
    #[allow(unused)]
    fn exists(field_attrs: &[Attribute]) -> bool {
        extract_openapi_attrs(field_attrs).any(|nested| {
            nested.len() == 1
                && match &nested[0] {
                    NestedMeta::Meta(Meta::Path(path)) => path.is_ident("debug"),
                    _ => false,
                }
        })
    }
}

/// Custom attribute that sets this attribute as required, even if the type is not required.
struct OpenApiRequired;

impl OpenApiRequired {
    fn exists(field_attrs: &[Attribute]) -> bool {
        extract_openapi_attrs(field_attrs).any(|nested| {
            nested.len() == 1
                && match &nested[0] {
                    NestedMeta::Meta(Meta::Path(path)) => path.is_ident("required"),
                    _ => false,
                }
        })
    }
}

/// Custom attribute that sets this attribute as optional, even if the type is not optional.
struct OpenApiOptional;

impl OpenApiOptional {
    fn exists(field_attrs: &[Attribute]) -> bool {
        extract_openapi_attrs(field_attrs).any(|nested| {
            nested.len() == 1
                && match &nested[0] {
                    NestedMeta::Meta(Meta::Path(path)) => path.is_ident("optional"),
                    _ => false,
                }
        })
    }
}

/// Custom attribute that sets this attribute as optional, even if the type is not optional.
struct OpenApiInline;

impl OpenApiInline {
    /// Traverses the field attributes and returns whether the field should be defined inline instead of
    /// as a reference.
    fn exists(field_attrs: &[Attribute]) -> bool {
        extract_openapi_attrs(field_attrs).any(|nested| {
            nested.len() == 1
                && match &nested[0] {
                    NestedMeta::Meta(Meta::Path(path)) => path.is_ident("inline"),
                    _ => false,
                }
        })
    }
}

#[derive(Clone, Debug, Default)]
struct SerdeProps {
    rename: Option<SerdeRename>,
    enum_tag_type: SerdeEnumTagType,
}

#[derive(Clone, Debug, PartialEq)]
enum SerdeEnumTagType {
    /// Default tag type. Enum variant name is used as the tag.
    External,
    /// When tag is provided and data tag is not
    Internal(String),
    /// When both tag and data tag are provided
    Adjacent(String, String),
    /// Enum variant name is not used.
    Untagged,
}

impl Default for SerdeEnumTagType {
    fn default() -> Self {
        SerdeEnumTagType::External
    }
}

impl SerdeProps {
    /// Traverses the serde attributes in the given item attributes and returns
    /// the applicable properties.
    fn from_item_attrs(item_attrs: &[Attribute]) -> Self {
        let mut props = Self::default();
        let mut enum_tag: Option<String> = None;
        let mut enum_content_tag: Option<String> = None;
        for meta in item_attrs.iter().filter_map(|a| a.parse_meta().ok()) {
            let inner_meta = match meta {
                Meta::List(ref l)
                    if l.path
                        .segments
                        .last()
                        .map(|p| p.ident == "serde")
                        .unwrap_or(false) =>
                {
                    &l.nested
                }
                _ => continue,
            };

            for meta in inner_meta {
                match meta {
                    NestedMeta::Meta(Meta::NameValue(ref v)) => {
                        if let Some(segment) = v.path.segments.last() {
                            match segment.ident.to_string().as_str() {
                                "rename_all" => {
                                    if let Lit::Str(ref s) = &v.lit {
                                        props.rename = s.value().parse().ok();
                                    }
                                }
                                "tag" => {
                                    if let Lit::Str(ref s) = &v.lit {
                                        enum_tag = Some(s.value());
                                    }
                                }
                                "content" => {
                                    if let Lit::Str(ref s) = &v.lit {
                                        enum_content_tag = Some(s.value());
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    NestedMeta::Meta(Meta::Path(syn::Path { segments, .. })) => {
                        if segments
                            .last()
                            .map(|p| p.ident == "untagged")
                            .unwrap_or(false)
                        {
                            props.enum_tag_type = SerdeEnumTagType::Untagged;
                        }
                    }
                    _ => continue,
                };
            }
        }

        if let Some(tag) = enum_tag {
            if let Some(content_tag) = enum_content_tag {
                props.enum_tag_type = SerdeEnumTagType::Adjacent(tag, content_tag);
            } else {
                props.enum_tag_type = SerdeEnumTagType::Internal(tag);
            }
        }

        props
    }
}

/// Supported flattening of embedded struct (https://serde.rs/variant-attrs.html).
struct SerdeFlatten;

impl SerdeFlatten {
    /// Traverses the field attributes and returns true if there is `#[serde(flatten)]`.
    fn exists(field_attrs: &[Attribute]) -> bool {
        for meta in field_attrs.iter().filter_map(|a| a.parse_meta().ok()) {
            let inner_meta = match meta {
                Meta::List(ref l)
                    if l.path
                        .segments
                        .last()
                        .map(|p| p.ident == "serde")
                        .unwrap_or(false) =>
                {
                    &l.nested
                }
                _ => continue,
            };

            for meta in inner_meta {
                if let NestedMeta::Meta(Meta::Path(syn::Path { segments, .. })) = meta {
                    if segments.iter().any(|p| p.ident == "flatten") {
                        return true;
                    }
                }
            }
        }

        false
    }
}

macro_rules! doc_comment {
    ($x:expr; $($tt:tt)*) => {
        #[doc = $x]
        $($tt)*
    };
}

#[cfg(feature = "actix")]
impl super::Method {
    fn handler_uri(attr: TokenStream) -> TokenStream {
        let attr = parse_macro_input!(attr as syn::AttributeArgs);
        attr.first().into_token_stream().into()
    }
    fn handler_name(item: TokenStream) -> syn::Result<syn::Ident> {
        let handler: ItemFn = syn::parse(item)?;
        Ok(handler.sig.ident)
    }
    pub(crate) fn generate(
        &self,
        attr: TokenStream,
        item: TokenStream,
    ) -> syn::Result<proc_macro2::TokenStream> {
        let uri: proc_macro2::TokenStream = Self::handler_uri(attr).into();
        let handler_name = Self::handler_name(item.clone())?;
        let handler_fn: proc_macro2::TokenStream = item.into();
        let method: proc_macro2::TokenStream = self.method().parse()?;
        let variant: proc_macro2::TokenStream = self.variant().parse()?;
        let handler_name_str = handler_name.to_string();

        let uri = uri.to_string().replace('\"', ""); // The uri is a string lit, which contains quotes, remove them

        let uri_fmt = if !uri.starts_with('/') {
            format!("/{}", uri)
        } else {
            uri
        };

        Ok(quote! {
            #[allow(non_camel_case_types, missing_docs)]
            pub struct #handler_name;

            impl #handler_name {
                fn resource() -> paperclip::actix::web::Resource {
                    #handler_fn
                    paperclip::actix::web::Resource::new(#uri_fmt)
                        .name(#handler_name_str)
                        .guard(actix_web::guard::#variant())
                        .route(paperclip::actix::web::#method().to(#handler_name))
                }
            }

            impl actix_web::dev::HttpServiceFactory for #handler_name {
                fn register(self, config: &mut actix_web::dev::AppService) {
                    Self::resource().register(config);
                }
            }

            impl paperclip::actix::Mountable for #handler_name {
                fn path(&self) -> &str {
                    #uri_fmt
                }

                fn operations(
                    &mut self,
                ) -> std::collections::BTreeMap<
                    paperclip::v2::models::HttpMethod,
                    paperclip::v2::models::DefaultOperationRaw,
                > {
                    Self::resource().operations()
                }

                fn definitions(
                    &mut self,
                ) -> std::collections::BTreeMap<
                    String,
                    paperclip::v2::models::DefaultSchemaRaw,
                > {
                    Self::resource().definitions()
                }

                fn security_definitions(
                    &mut self,
                ) -> std::collections::BTreeMap<String, paperclip::v2::models::SecurityScheme>
                {
                    Self::resource().security_definitions()
                }
            }
        })
    }
}

macro_rules! rest_methods {
    (
        $($variant:ident, $method:ident, )+
    ) => {
        /// All available Rest methods
        #[derive(Debug, PartialEq, Eq, Hash)]
        pub(crate) enum Method {
            $(
                $variant,
            )+
        }

        impl Method {
            fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($method),)+
                }
            }
            fn variant(&self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)+
                }
            }
        }

        $(doc_comment! {
            concat!("
Creates route handler with `paperclip::actix::web::Resource", "`.
In order to control the output type and status codes the return value/response must implement the
trait actix_web::Responder.

# Syntax
```text
#[", stringify!($method), r#"("path"[, attributes])]
```

# Attributes
- `"path"` - Raw literal string with path for which to register handler.

# Example

/// use paperclip::actix::web::Json;
/// use paperclip_macros::"#, stringify!($method), ";
/// #[", stringify!($method), r#"("/")]
/// async fn example() {
/// }
"#);
            #[cfg(feature = "actix")]
            #[proc_macro_attribute]
            pub fn $method(attr: TokenStream, item: TokenStream) -> TokenStream {
                match Method::$variant.generate(attr, item) {
                    Ok(v) => v.into(),
                    Err(e) => e.to_compile_error().into(),
                }
            }
        })+
    };
}
