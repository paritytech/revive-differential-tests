use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::Visibility;

use super::types::{EventDef, EventGroup, RunnerEventInput};

pub(crate) fn codegen(input: &RunnerEventInput) -> TokenStream {
    let vis = &input.vis;
    let enum_ident = &input.enum_ident;

    // Collect (group, event) pairs for enum-wide generation.
    let all_events: Vec<(&EventGroup, &EventDef)> = input
        .groups
        .iter()
        .flat_map(|group| group.events.iter().map(move |event| (group, event)))
        .collect();

    let enum_attrs = &input.attrs;
    let event_structs = gen_event_structs(vis, &input.groups);
    let enum_def = gen_enum(enum_attrs, vis, enum_ident, &all_events);
    let from_impls = gen_from_impls(enum_ident, &all_events);
    let reporters = gen_reporters(vis, enum_ident, &input.groups);

    quote! {
        #event_structs
        #enum_def
        #from_impls
        #reporters
    }
}

/// Convert a PascalCase identifier to snake_case.
pub(super) fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push('_');
        }
        result.extend(c.to_lowercase());
    }
    result
}

/// Derive the reporter type name from a group key.
///
/// - `Reporter` → `Reporter`
/// - `TestSpecifier` → `TestSpecificReporter`
/// - `ExecutionSpecifier` → `ExecutionSpecificReporter`
fn reporter_type_name(key: &Ident) -> Ident {
    let key_str = key.to_string();
    if key_str == "Reporter" {
        format_ident!("Reporter")
    } else if let Some(base) = key_str.strip_suffix("Specifier") {
        format_ident!("{}SpecificReporter", base)
    } else {
        format_ident!("{}SpecificReporter", key_str)
    }
}

// ---------------------------------------------------------------------------
// Event structs
// ---------------------------------------------------------------------------

fn gen_event_structs(vis: &Visibility, groups: &[EventGroup]) -> TokenStream {
    let structs: Vec<TokenStream> = groups
        .iter()
        .flat_map(|group| {
            let is_base = group.key == "Reporter";
            let specifier_field_name = format_ident!("{}", to_snake_case(&group.key.to_string()));
            let specifier_type = &group.key;

            group.events.iter().map(move |event| {
                let struct_name = format_ident!("{}Event", event.ident);
                let attrs = &event.attrs;
                let field_tokens = gen_field_definitions(vis, &event.fields);

                if is_base {
                    quote! {
                        #(#attrs)*
                        #[derive(Debug)]
                        #vis struct #struct_name {
                            #field_tokens
                        }
                    }
                } else {
                    quote! {
                        #(#attrs)*
                        #[derive(Debug)]
                        #vis struct #struct_name {
                            #vis #specifier_field_name: std::sync::Arc<#specifier_type>,
                            #field_tokens
                        }
                    }
                }
            })
        })
        .collect();

    quote! { #(#structs)* }
}

fn gen_field_definitions(vis: &Visibility, fields: &[super::types::EventField]) -> TokenStream {
    let tokens: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let attrs = &field.attrs;
            let ident = &field.ident;
            let ty = &field.ty;
            quote! {
                #(#attrs)*
                #vis #ident: #ty,
            }
        })
        .collect();
    quote! { #(#tokens)* }
}

// ---------------------------------------------------------------------------
// Enum definition + variant_name()
// ---------------------------------------------------------------------------

fn gen_enum(
    enum_attrs: &[syn::Attribute],
    vis: &Visibility,
    enum_ident: &Ident,
    all_events: &[(&EventGroup, &EventDef)],
) -> TokenStream {
    let variant_idents: Vec<&Ident> = all_events.iter().map(|(_, e)| &e.ident).collect();
    let struct_names: Vec<Ident> = all_events
        .iter()
        .map(|(_, e)| format_ident!("{}Event", e.ident))
        .collect();

    quote! {
        #(#enum_attrs)*
        #[derive(Debug)]
        #vis enum #enum_ident {
            #(
                #variant_idents(Box<#struct_names>),
            )*
        }

        impl #enum_ident {
            pub fn variant_name(&self) -> &'static str {
                match self {
                    #(
                        Self::#variant_idents { .. } => stringify!(#variant_idents),
                    )*
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// From impls
// ---------------------------------------------------------------------------

fn gen_from_impls(enum_ident: &Ident, all_events: &[(&EventGroup, &EventDef)]) -> TokenStream {
    let impls: Vec<TokenStream> = all_events
        .iter()
        .map(|(_, event)| {
            let variant_ident = &event.ident;
            let struct_name = format_ident!("{}Event", event.ident);

            quote! {
                impl From<#struct_name> for #enum_ident {
                    fn from(value: #struct_name) -> Self {
                        Self::#variant_ident(Box::new(value))
                    }
                }
            }
        })
        .collect();

    quote! { #(#impls)* }
}

// ---------------------------------------------------------------------------
// Reporter structs + methods
// ---------------------------------------------------------------------------

fn gen_reporters(vis: &Visibility, enum_ident: &Ident, groups: &[EventGroup]) -> TokenStream {
    let base_reporter_name = format_ident!("Reporter");

    let reporters: Vec<TokenStream> = groups
        .iter()
        .map(|group| {
            let is_base = group.key == "Reporter";
            let reporter_name = reporter_type_name(&group.key);
            let specifier_field_name = format_ident!("{}", to_snake_case(&group.key.to_string()));
            let specifier_type = &group.key;

            // Reporter structs are `pub` so they can be used across crates.
            // Their fields use the input visibility (typically `pub(crate)`).
            let struct_def = if is_base {
                quote! {
                    #[derive(Clone, Debug)]
                    pub struct #reporter_name(
                        #vis tokio::sync::mpsc::UnboundedSender<#enum_ident>
                    );

                    impl From<tokio::sync::mpsc::UnboundedSender<#enum_ident>> for #reporter_name {
                        fn from(value: tokio::sync::mpsc::UnboundedSender<#enum_ident>) -> Self {
                            Self(value)
                        }
                    }
                }
            } else {
                quote! {
                    #[derive(Clone, Debug)]
                    pub struct #reporter_name {
                        #vis reporter: #base_reporter_name,
                        #vis #specifier_field_name: std::sync::Arc<#specifier_type>,
                    }
                }
            };

            // report() helper
            let report_method = if is_base {
                quote! {
                    fn report(&self, event: impl Into<#enum_ident>) -> anyhow::Result<()> {
                        self.0.send(event.into()).map_err(Into::into)
                    }
                }
            } else {
                quote! {
                    fn report(&self, event: impl Into<#enum_ident>) -> anyhow::Result<()> {
                        self.reporter.report(event)
                    }
                }
            };

            // report_*_event methods
            let event_methods: Vec<TokenStream> = group
                .events
                .iter()
                .map(|event| gen_report_method(is_base, event, &specifier_field_name))
                .collect();

            quote! {
                #struct_def

                impl #reporter_name {
                    #report_method
                    #(#event_methods)*
                }
            }
        })
        .collect();

    quote! { #(#reporters)* }
}

fn gen_report_method(is_base: bool, event: &EventDef, specifier_field_name: &Ident) -> TokenStream {
    let method_name = format_ident!("report_{}_event", to_snake_case(&event.ident.to_string()));
    let struct_name = format_ident!("{}Event", event.ident);

    let param_list: Vec<TokenStream> = event
        .fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            quote! { #ident: impl Into<#ty> }
        })
        .collect();

    let field_inits: Vec<TokenStream> = event
        .fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            quote! { #ident: #ident.into() }
        })
        .collect();

    if is_base {
        quote! {
            pub fn #method_name(&self #(, #param_list)*) -> anyhow::Result<()> {
                self.report(#struct_name {
                    #(#field_inits,)*
                })
            }
        }
    } else {
        quote! {
            pub fn #method_name(&self #(, #param_list)*) -> anyhow::Result<()> {
                self.report(#struct_name {
                    #specifier_field_name: self.#specifier_field_name.clone(),
                    #(#field_inits,)*
                })
            }
        }
    }
}
