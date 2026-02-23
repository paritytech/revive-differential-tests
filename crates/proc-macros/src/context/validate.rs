use syn::{Attribute, Item, ItemMod, Path, spanned::Spanned};

use super::parse::{TypeKind, classify_type, type_name};
use super::types::{Configuration, ContextArgs, Subcommand, TypeDef, ValidatedModule};

fn is_context_attr(attr: &Attribute) -> bool {
    attr.path().is_ident("subcommand") || attr.path().is_ident("configuration")
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

        // Check if any existing clap/arg attribute already has `long` or `id`
        let has_explicit_long_or_id = field.attrs.iter().any(|attr| {
            if !attr.path().is_ident("clap") && !attr.path().is_ident("arg") {
                return false;
            }
            let mut found = false;
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("long") || meta.path.is_ident("id") {
                    found = true;
                }
                // Skip the value if present
                if meta.input.peek(syn::Token![=]) {
                    meta.input.parse::<syn::Token![=]>()?;
                    let _ = meta.input.parse::<syn::Lit>()?;
                }
                Ok(())
            });
            found
        });

        if has_explicit_long_or_id {
            continue;
        }

        // Generate "key.field-name" (underscore → hyphen)
        let prefixed_name = format!("{}.{}", key, field_ident.to_string().replace('_', "-"));
        field
            .attrs
            .push(syn::parse_quote! { #[clap(id = #prefixed_name, long = #prefixed_name)] });
    }
}

/// For each subcommand field whose type matches a configuration with a `help_heading`,
/// add `#[clap(next_help_heading = "...")]` if the field has `#[clap(flatten)]` and
/// doesn't already have `next_help_heading`.
fn apply_help_headings(
    subcommands: &mut [super::types::Subcommand],
    configurations: &[super::types::Configuration],
) {
    use super::parse::type_name;

    // Build a map: config type name → help_heading
    let config_headings: std::collections::HashMap<String, &str> = configurations
        .iter()
        .filter_map(|c| {
            c.args
                .help_heading
                .as_ref()
                .map(|h| (c.type_def.ident().to_string(), h.as_str()))
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
            // Look up the config heading for this field's type
            let Some(field_ty_name) = type_name(&field.ty) else {
                continue;
            };
            let Some(heading) = config_headings.get(&field_ty_name) else {
                continue;
            };

            // Check if any existing clap/arg attribute already has next_help_heading
            let has_heading = field.attrs.iter().any(|attr| {
                if !attr.path().is_ident("clap") && !attr.path().is_ident("arg") {
                    return false;
                }
                let mut found = false;
                let _ = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("next_help_heading") {
                        found = true;
                    }
                    // Skip value if present
                    if meta.input.peek(syn::Token![=]) {
                        meta.input.parse::<syn::Token![=]>()?;
                        let _ = meta.input.parse::<syn::Lit>()?;
                    }
                    Ok(())
                });
                found
            });

            if !has_heading {
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

    // First pass: collect all type names defined in the module.
    let mut defined_type_names = std::collections::HashSet::new();
    for item in &items {
        match item {
            Item::Struct(s) => {
                defined_type_names.insert(s.ident.to_string());
            }
            Item::Enum(e) => {
                defined_type_names.insert(e.ident.to_string());
            }
            _ => {}
        }
    }

    let mut subcommands = Vec::new();
    let mut configurations = Vec::new();
    let mut impls = Vec::new();
    let mut uses = Vec::new();

    // Second pass: validate every item and collect into typed output.
    for item in items {
        match item {
            Item::Struct(s) => {
                let Some(kind) = classify_type(&s.attrs) else {
                    return Err(syn::Error::new(
                        s.ident.span(),
                        format!(
                            "struct `{}` must have a `#[subcommand]` or `#[configuration]` attribute",
                            s.ident
                        ),
                    ));
                };
                let mut type_def = TypeDef::Struct(s);
                strip_context_attrs(&mut type_def);
                apply_default_derives(&mut type_def, &args.default_derives);
                match kind {
                    TypeKind::Subcommand(sub_args) => subcommands.push(Subcommand {
                        type_def,
                        args: sub_args,
                    }),
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
            Item::Enum(e) => {
                let Some(kind) = classify_type(&e.attrs) else {
                    return Err(syn::Error::new(
                        e.ident.span(),
                        format!(
                            "enum `{}` must have a `#[subcommand]` or `#[configuration]` attribute",
                            e.ident
                        ),
                    ));
                };
                let mut type_def = TypeDef::Enum(e);
                strip_context_attrs(&mut type_def);
                apply_default_derives(&mut type_def, &args.default_derives);
                match kind {
                    TypeKind::Subcommand(sub_args) => subcommands.push(Subcommand {
                        type_def,
                        args: sub_args,
                    }),
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
                let self_ty_name = type_name(&impl_block.self_ty);
                match self_ty_name {
                    Some(name) if defined_type_names.contains(&name) => {}
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
