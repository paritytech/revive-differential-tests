use syn::{Ident, Path, Token, parse::Parse};

use super::types::{ConfigurationArgs, ContextArgs, SubcommandArgs};

pub(crate) enum TypeKind {
    Subcommand(SubcommandArgs),
    Configuration(ConfigurationArgs),
}

/// Determine whether the attributes contain `subcommand` or `configuration`.
/// For `#[configuration(key = "...")]`, parse the key argument.
pub(crate) fn classify_type(attrs: &[syn::Attribute]) -> Option<TypeKind> {
    for attr in attrs {
        if attr.path().is_ident("subcommand") {
            return Some(TypeKind::Subcommand(SubcommandArgs {}));
        }
        if attr.path().is_ident("configuration") {
            let mut key = None;
            let mut help_heading = None;
            // Try to parse arguments: #[configuration(key = "...", help_heading = "...")]
            let _ = attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("key") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    key = Some(lit.value());
                } else if meta.path.is_ident("help_heading") {
                    let value = meta.value()?;
                    let lit: syn::LitStr = value.parse()?;
                    help_heading = Some(lit.value());
                }
                Ok(())
            });
            // If help_heading not explicitly set, derive from key by title-casing.
            if help_heading.is_none() {
                help_heading = key.as_ref().map(|k| derive_help_heading(k));
            }
            return Some(TypeKind::Configuration(ConfigurationArgs {
                key,
                help_heading,
            }));
        }
    }
    None
}

/// Derive a help heading from a configuration key.
/// E.g. "solc" → "Solc Configuration", "revive-dev-node" → "Revive Dev Node Configuration".
fn derive_help_heading(key: &str) -> String {
    let words: Vec<String> = key
        .split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let mut s = c.to_uppercase().to_string();
                    s.push_str(chars.as_str());
                    s
                }
            }
        })
        .collect();
    format!("{} Configuration", words.join(" "))
}

/// Extract the simple type name from a `syn::Type` (handles paths like `Self` or `Foo`).
pub(crate) fn type_name(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(type_path) = ty {
        type_path.path.get_ident().map(|id| id.to_string())
    } else {
        None
    }
}

impl Parse for ContextArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut context_type_ident = None;
        let mut default_derives = Vec::new();

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            if key == "context_type_ident" {
                input.parse::<Token![=]>()?;
                let lit: syn::LitStr = input.parse()?;
                context_type_ident = Some(lit.parse::<Ident>()?);
            } else if key == "default_derives" {
                input.parse::<Token![=]>()?;
                let lit: syn::LitStr = input.parse()?;
                default_derives = lit
                    .value()
                    .split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .map(syn::parse_str::<Path>)
                    .collect::<syn::Result<Vec<_>>>()?;
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    format!("unknown attribute `{key}`"),
                ));
            }

            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(Self {
            context_type_ident,
            default_derives,
        })
    }
}
