use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use super::types::{ConfigMembership, Configuration, Subcommand, SubcommandConfigField};

/// For each configuration, determine which subcommands have it as a field and which don't.
pub(crate) fn compute_config_membership<'a>(
    subcommands: &'a [Subcommand],
    configurations: &'a [Configuration],
) -> syn::Result<Vec<ConfigMembership<'a>>> {
    let mut memberships = Vec::new();

    for config in configurations {
        let config_ident = config.type_def.ident();
        let config_name = config_ident.to_string();
        let mut present_in = Vec::new();
        let mut absent_from = Vec::new();

        for subcmd in subcommands {
            let subcmd_ident = subcmd.type_def.ident();
            let named = subcmd.type_def.named_fields();

            let mut matches: Vec<&Ident> = Vec::new();
            for (field_ident, field_ty) in &named {
                if let syn::Type::Path(type_path) = field_ty {
                    if let Some(ident) = type_path.path.get_ident() {
                        if *ident == config_name {
                            matches.push(field_ident);
                        }
                    }
                }
            }

            if matches.len() > 1 {
                return Err(syn::Error::new_spanned(
                    subcmd_ident,
                    format!(
                        "subcommand `{}` has multiple fields of type `{}`",
                        subcmd_ident, config_name
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
    let mut statics = TokenStream::new();

    for membership in memberships {
        if membership.absent_from.is_empty() {
            continue;
        }

        let config_ident = membership.config_ident;
        let static_ident = default_static_ident(config_ident);

        statics.extend(quote! {
            static #static_ident: std::sync::LazyLock<#config_ident> =
                std::sync::LazyLock::new(|| {
                    <#config_ident as clap::Parser>::parse_from(std::iter::empty::<std::ffi::OsString>())
                });
        });
    }

    statics
}

/// Generate `AsRef<Config>` impls on ALL subcommands for ALL configs.
pub(crate) fn gen_subcommand_as_ref_impls(memberships: &[ConfigMembership]) -> TokenStream {
    let mut impls = TokenStream::new();

    for membership in memberships {
        let config_ident = membership.config_ident;

        // Subcommands that HAVE the config field
        for scf in &membership.present_in {
            let subcmd_ident = scf.subcommand_ident;
            let field_ident = scf.field_ident;
            impls.extend(quote! {
                impl AsRef<#config_ident> for #subcmd_ident {
                    fn as_ref(&self) -> &#config_ident {
                        &self.#field_ident
                    }
                }
            });
        }

        // Subcommands that DO NOT have the config field — reference the default static
        if !membership.absent_from.is_empty() {
            let static_ident = default_static_ident(config_ident);
            for subcmd_ident in &membership.absent_from {
                impls.extend(quote! {
                    impl AsRef<#config_ident> for #subcmd_ident {
                        fn as_ref(&self) -> &#config_ident {
                            &#static_ident
                        }
                    }
                });
            }
        }
    }

    impls
}

/// Generate `AsRef<Config>` impls on the context enum. Every subcommand already implements
/// `AsRef<Config>` for every config, so the context enum simply delegates.
pub(crate) fn gen_context_as_ref_impls(
    context_ident: &Ident,
    subcommands: &[Subcommand],
    memberships: &[ConfigMembership],
) -> TokenStream {
    let mut impls = TokenStream::new();

    let subcmd_idents: Vec<&Ident> = subcommands.iter().map(|s| s.type_def.ident()).collect();

    for membership in memberships {
        let config_ident = membership.config_ident;
        let arms = subcmd_idents.iter().map(|subcmd_ident| {
            quote! { Self::#subcmd_ident(ctx) => <#subcmd_ident as AsRef<#config_ident>>::as_ref(ctx) }
        });

        impls.extend(quote! {
            impl AsRef<#config_ident> for #context_ident {
                fn as_ref(&self) -> &#config_ident {
                    match self {
                        #(#arms),*
                    }
                }
            }
        });
    }

    impls
}

/// Compute the static identifier name for a config type's default.
/// E.g. `SolcConfiguration` → `DEFAULT_SOLC_CONFIGURATION`
fn default_static_ident(config_ident: &Ident) -> Ident {
    let name = config_ident.to_string();
    let mut screaming = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            screaming.push('_');
        }
        screaming.push(ch.to_ascii_uppercase());
    }
    format_ident!("DEFAULT_{}", screaming)
}
