use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use super::parse::type_ident;
use super::types::{ConfigMembership, Configuration, Subcommand, SubcommandConfigField};

/// For each configuration, determine which subcommands have it as a field and which don't.
pub(crate) fn compute_config_membership<'a>(
    subcommands: &'a [Subcommand],
    configurations: &'a [Configuration],
) -> syn::Result<Vec<ConfigMembership<'a>>> {
    let mut memberships = Vec::new();

    for config in configurations {
        let config_ident = config.type_def.ident();
        let mut present_in = Vec::new();
        let mut absent_from = Vec::new();

        for subcmd in subcommands {
            let subcmd_ident = subcmd.type_def.ident();
            let named = subcmd.type_def.named_fields();

            let mut matches: Vec<&Ident> = Vec::new();
            for (field_ident, field_ty) in &named {
                if let Some(ident) = type_ident(field_ty) {
                    if ident == config_ident {
                        matches.push(field_ident);
                    }
                }
            }

            if matches.len() > 1 {
                return Err(syn::Error::new_spanned(
                    subcmd_ident,
                    format!(
                        "subcommand `{}` has multiple fields of type `{}`",
                        subcmd_ident, config_ident
                    ),
                ));
            }

            if let Some(field_ident) = matches.into_iter().next() {
                present_in.push(SubcommandConfigField {
                    subcommand_ident: subcmd_ident,
                    field_ident,
                });
            } else {
                absent_from.push(subcmd_ident);
            }
        }

        memberships.push(ConfigMembership {
            config_ident,
            present_in,
            absent_from,
        });
    }

    Ok(memberships)
}

/// Generate top-level `LazyLock` default statics for configurations that are missing from some
/// subcommands.
pub(crate) fn gen_default_statics(memberships: &[ConfigMembership]) -> TokenStream {
    let statics: Vec<_> = memberships
        .iter()
        .filter(|m| !m.absent_from.is_empty())
        .map(|m| {
            let config_ident = m.config_ident;
            let static_ident = default_static_ident(config_ident);
            quote! {
                static #static_ident: std::sync::LazyLock<#config_ident> =
                    std::sync::LazyLock::new(|| {
                        <#config_ident as clap::Parser>::parse_from(std::iter::empty::<std::ffi::OsString>())
                    });
            }
        })
        .collect();
    quote! { #(#statics)* }
}

/// Generate `AsRef<Config>` impls on ALL subcommands for ALL configs.
pub(crate) fn gen_subcommand_as_ref_impls(memberships: &[ConfigMembership]) -> TokenStream {
    let impls: Vec<_> = memberships
        .iter()
        .flat_map(|m| {
            let config_ident = m.config_ident;

            let present: Vec<_> = m
                .present_in
                .iter()
                .map(|scf| {
                    let subcmd_ident = scf.subcommand_ident;
                    let field_ident = scf.field_ident;
                    quote! {
                        impl AsRef<#config_ident> for #subcmd_ident {
                            fn as_ref(&self) -> &#config_ident {
                                &self.#field_ident
                            }
                        }
                    }
                })
                .collect();

            let absent: Vec<_> = if m.absent_from.is_empty() {
                Vec::new()
            } else {
                let static_ident = default_static_ident(config_ident);
                m.absent_from
                    .iter()
                    .map(|subcmd_ident| {
                        quote! {
                            impl AsRef<#config_ident> for #subcmd_ident {
                                fn as_ref(&self) -> &#config_ident {
                                    &#static_ident
                                }
                            }
                        }
                    })
                    .collect()
            };

            present.into_iter().chain(absent)
        })
        .collect();
    quote! { #(#impls)* }
}

/// Generate `AsRef<Config>` impls on the context enum. Every subcommand already implements
/// `AsRef<Config>` for every config, so the context enum simply delegates.
pub(crate) fn gen_context_as_ref_impls(
    context_ident: &Ident,
    subcommands: &[Subcommand],
    memberships: &[ConfigMembership],
) -> TokenStream {
    let subcmd_idents: Vec<&Ident> = subcommands.iter().map(|s| s.type_def.ident()).collect();

    let impls: Vec<_> = memberships
        .iter()
        .map(|m| {
            let config_ident = m.config_ident;
            let arms = subcmd_idents.iter().map(|subcmd_ident| {
                quote! { Self::#subcmd_ident(ctx) => <#subcmd_ident as AsRef<#config_ident>>::as_ref(ctx) }
            });
            quote! {
                impl AsRef<#config_ident> for #context_ident {
                    fn as_ref(&self) -> &#config_ident {
                        match self {
                            #(#arms),*
                        }
                    }
                }
            }
        })
        .collect();
    quote! { #(#impls)* }
}

/// Generate `Has<Config>` traits and implement them on all subcommands and the context enum.
///
/// For each configuration type (e.g. `SolcConfiguration`), this generates:
/// 1. A public trait `HasSolcConfiguration` with method `fn as_solc_configuration(&self) -> &SolcConfiguration`
/// 2. An impl on every subcommand that delegates to `AsRef`
/// 3. An impl on the context enum that delegates to `AsRef`
pub(crate) fn gen_has_config_traits(
    context_ident: &Ident,
    subcommands: &[Subcommand],
    memberships: &[ConfigMembership],
) -> TokenStream {
    let subcmd_idents: Vec<&Ident> = subcommands.iter().map(|s| s.type_def.ident()).collect();

    let traits_and_impls: Vec<_> = memberships
        .iter()
        .map(|m| {
            let config_ident = m.config_ident;
            let config_name = config_ident.to_string();
            let trait_ident = format_ident!("Has{}", config_name);
            let method_ident = format_ident!("as_{}", pascal_to_snake(&config_name));

            let subcmd_impls = subcmd_idents.iter().map(|subcmd_ident| {
                quote! {
                    impl #trait_ident for #subcmd_ident {
                        fn #method_ident(&self) -> &#config_ident {
                            self.as_ref()
                        }
                    }
                }
            });

            quote! {
                pub trait #trait_ident {
                    fn #method_ident(&self) -> &#config_ident;
                }

                #(#subcmd_impls)*

                impl #trait_ident for #context_ident {
                    fn #method_ident(&self) -> &#config_ident {
                        self.as_ref()
                    }
                }
            }
        })
        .collect();
    quote! { #(#traits_and_impls)* }
}

/// Shared core for PascalCase → separated case conversion.
fn pascal_case_separated(name: &str, separator: char, to_case: fn(char) -> char) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push(separator);
        }
        result.push(to_case(ch));
    }
    result
}

/// Convert a PascalCase name to snake_case.
/// E.g. `SolcConfiguration` → `solc_configuration`
fn pascal_to_snake(name: &str) -> String {
    pascal_case_separated(name, '_', |c| c.to_ascii_lowercase())
}

/// Compute the static identifier name for a config type's default.
/// E.g. `SolcConfiguration` → `DEFAULT_SOLC_CONFIGURATION`
fn default_static_ident(config_ident: &Ident) -> Ident {
    let screaming =
        pascal_case_separated(&config_ident.to_string(), '_', |c| c.to_ascii_uppercase());
    format_ident!("DEFAULT_{}", screaming)
}
