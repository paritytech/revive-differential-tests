use syn::{Attribute, Ident, Item, ItemMod, Path, spanned::Spanned};

use super::parse::{TypeKind, classify_type, type_ident};
use super::types::{Configuration, ContextArgs, Subcommand, TypeDef, ValidatedModule};

fn is_context_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("subcommand") || attr.path().is_ident("configuration")
}

/// Check if a field has a `#[clap(...)]` or `#[arg(...)]` attribute containing any of the
/// given nested meta names.
fn has_clap_attr(field: &syn::Field, names: &[&str]) -> bool {
    field.attrs.iter().any(|attr| {
        if !attr.path().is_ident("clap") && !attr.path().is_ident("arg") {
            return false;
        }
        let mut found = false;
        let _ = attr.parse_nested_meta(|meta| {
            if names.iter().any(|name| meta.path.is_ident(name)) {
                found = true;
            }
            if meta.input.peek(syn::Token![=]) {
                meta.input.parse::<syn::Token![=]>()?;
                let _ = meta.input.parse::<syn::Lit>()?;
            }
            Ok(())
        });
        found
    })
}

/// Strip `#[subcommand]` and `#[configuration]` attributes from a type definition.
pub(crate) fn strip_context_attrs(type_def: &mut TypeDef) {
    type_def.attrs_mut().retain(|attr| !is_context_attr(attr));
}

/// Merge `default_derives` into a type definition's attributes. If an existing `#[derive(...)]`
/// is found, the defaults are prepended to it. Otherwise a new `#[derive(...)]` is added.
pub(crate) fn apply_default_derives(type_def: &mut TypeDef, default_derives: &[Path]) {
    if default_derives.is_empty() {
        return;
    }

    let attrs = type_def.attrs_mut();

    // Look for an existing #[derive(...)] attribute.
    let existing = attrs.iter_mut().find(|attr| attr.path().is_ident("derive"));

    if let Some(attr) = existing {
        // Parse existing derives, prepend defaults, and rebuild the attribute.
        let mut all_paths: Vec<Path> = default_derives.to_vec();
        let _ = attr.parse_nested_meta(|meta| {
            all_paths.push(meta.path);
            Ok(())
        });
        *attr = syn::parse_quote! { #[derive(#(#all_paths),*)] };
    } else {
        let paths = default_derives;
        attrs.insert(0, syn::parse_quote! { #[derive(#(#paths),*)] });
    }
}

/// For each named field in the struct, if the field does NOT already have an explicit `long` or
/// `id` in its `#[clap(...)]`/`#[arg(...)]` attributes, generate `id = "key.field-name"` and
/// `long = "key.field-name"` (underscore to hyphen conversion) and add as a
/// `#[clap(id = "...", long = "...")]` attribute on the field.
pub(crate) fn apply_config_key_prefixes(type_def: &mut TypeDef, key: &str) {
    let fields = match type_def {
        TypeDef::Struct(item) => &mut item.fields,
        TypeDef::Enum(_) => return,
    };

    for field in fields.iter_mut() {
        let Some(field_ident) = &field.ident else {
            continue;
        };

        if has_clap_attr(field, &["long", "id", "skip"]) {
            continue;
        }

        // Generate "key.field-name" (underscore → hyphen)
        let prefixed_name = format!("{}.{}", key, field_ident.to_string().replace('_', "-"));
        field
            .attrs
            .push(syn::parse_quote! { #[clap(id = #prefixed_name, long = #prefixed_name)] });
    }
}

/// For each subcommand field whose type matches a configuration type name,
/// auto-add `#[clap(flatten)]` if the field doesn't already have it.
fn apply_auto_flatten(
    subcommands: &mut [super::types::Subcommand],
    configurations: &[super::types::Configuration],
) {
    let config_idents: std::collections::HashSet<&Ident> =
        configurations.iter().map(|c| c.type_def.ident()).collect();

    if config_idents.is_empty() {
        return;
    }

    for subcmd in subcommands {
        let fields = match &mut subcmd.type_def {
            TypeDef::Struct(s) => &mut s.fields,
            TypeDef::Enum(_) => continue,
        };

        for field in fields.iter_mut() {
            let Some(field_ty_ident) = type_ident(&field.ty) else {
                continue;
            };
            if !config_idents.contains(field_ty_ident) {
                continue;
            }

            if !has_clap_attr(field, &["flatten"]) {
                field.attrs.push(syn::parse_quote! { #[clap(flatten)] });
            }
        }
    }
}

/// For each subcommand field whose type matches a configuration with a `help_heading`,
/// add `#[clap(next_help_heading = "...")]` if the field has `#[clap(flatten)]` and
/// doesn't already have `next_help_heading`.
fn apply_help_headings(
    subcommands: &mut [super::types::Subcommand],
    configurations: &[super::types::Configuration],
) {
    // Build a map: config type ident → help_heading
    let config_headings: std::collections::HashMap<&Ident, &str> = configurations
        .iter()
        .filter_map(|c| {
            c.args
                .help_heading
                .as_ref()
                .map(|h| (c.type_def.ident(), h.as_str()))
        })
        .collect();

    if config_headings.is_empty() {
        return;
    }

    for subcmd in subcommands {
        let fields = match &mut subcmd.type_def {
            TypeDef::Struct(s) => &mut s.fields,
            TypeDef::Enum(_) => continue,
        };

        for field in fields.iter_mut() {
            let Some(field_ty_ident) = type_ident(&field.ty) else {
                continue;
            };
            let Some(heading) = config_headings.get(field_ty_ident) else {
                continue;
            };

            if !has_clap_attr(field, &["next_help_heading"]) {
                field
                    .attrs
                    .push(syn::parse_quote! { #[clap(next_help_heading = #heading)] });
            }
        }
    }
}

pub(crate) fn validate(module: ItemMod, args: &ContextArgs) -> syn::Result<ValidatedModule> {
    let Some((_, items)) = module.content else {
        return Err(syn::Error::new(
            module.span(),
            "the `context` attribute requires a module with inline content (not `mod foo;`)",
        ));
    };

    // First pass: collect all type idents defined in the module.
    let mut defined_type_idents: std::collections::HashSet<Ident> =
        std::collections::HashSet::new();
    for item in &items {
        match item {
            Item::Struct(s) => {
                defined_type_idents.insert(s.ident.clone());
            }
            Item::Enum(e) => {
                defined_type_idents.insert(e.ident.clone());
            }
            _ => {}
        }
    }

    // Also register the context type ident so that `impl ContextV2 { ... }` is valid.
    defined_type_idents.insert(args.context_type_ident().clone());

    let mut subcommands = Vec::new();
    let mut configurations = Vec::new();
    let mut impls = Vec::new();
    let mut uses = Vec::new();

    // Second pass: validate every item and collect into typed output.
    for item in items {
        match item {
            item @ (Item::Struct(_) | Item::Enum(_)) => {
                let type_def = match item {
                    Item::Struct(s) => TypeDef::Struct(s),
                    Item::Enum(e) => TypeDef::Enum(e),
                    _ => unreachable!(),
                };
                let Some(kind) = classify_type(type_def.attrs()) else {
                    return Err(syn::Error::new(
                        type_def.ident().span(),
                        format!(
                            "`{}` must have a `#[subcommand]` or `#[configuration]` attribute",
                            type_def.ident()
                        ),
                    ));
                };
                let mut type_def = type_def;
                strip_context_attrs(&mut type_def);
                apply_default_derives(&mut type_def, &args.default_derives);
                match kind {
                    TypeKind::Subcommand => subcommands.push(Subcommand { type_def }),
                    TypeKind::Configuration(config_args) => {
                        if let Some(key) = &config_args.key {
                            apply_config_key_prefixes(&mut type_def, key);
                        }
                        configurations.push(Configuration {
                            type_def,
                            args: config_args,
                        })
                    }
                }
            }
            Item::Impl(impl_block) => {
                let self_ty_ident = type_ident(&impl_block.self_ty);
                match self_ty_ident {
                    Some(ident) if defined_type_idents.contains(ident) => {}
                    _ => {
                        return Err(syn::Error::new(
                            impl_block.self_ty.span(),
                            "only impls for types defined within this module are allowed",
                        ));
                    }
                }
                impls.push(impl_block);
            }
            Item::Use(use_item) => {
                uses.push(use_item);
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "only struct definitions, enum definitions, impls, and use statements are allowed inside a `#[context]` module",
                ));
            }
        }
    }

    // Post-process: auto-add #[clap(flatten)] on subcommand fields that reference configs.
    apply_auto_flatten(&mut subcommands, &configurations);

    // Post-process: inject next_help_heading on subcommand fields that reference configs.
    apply_help_headings(&mut subcommands, &configurations);

    Ok(ValidatedModule {
        subcommands,
        configurations,
        impls,
        uses,
        module_attributes: module.attrs,
    })
}
