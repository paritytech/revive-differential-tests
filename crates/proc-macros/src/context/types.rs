use proc_macro2::TokenStream;
use quote::ToTokens;
use syn::{Attribute, Ident, ItemEnum, ItemImpl, ItemStruct, Path};

pub(crate) enum TypeDef {
    Struct(ItemStruct),
    Enum(ItemEnum),
}

impl TypeDef {
    pub(crate) fn ident(&self) -> &Ident {
        match self {
            Self::Struct(item) => &item.ident,
            Self::Enum(item) => &item.ident,
        }
    }

    pub(crate) fn field_types(&self) -> Vec<&syn::Type> {
        match self {
            Self::Struct(item) => item.fields.iter().map(|f| &f.ty).collect(),
            Self::Enum(item) => item
                .variants
                .iter()
                .flat_map(|v| v.fields.iter().map(|f| &f.ty))
                .collect(),
        }
    }

    pub(crate) fn doc_attrs(&self) -> Vec<&Attribute> {
        let attrs = match self {
            Self::Struct(item) => &item.attrs,
            Self::Enum(item) => &item.attrs,
        };
        attrs
            .iter()
            .filter(|attr| attr.path().is_ident("doc"))
            .collect()
    }

    pub(crate) fn attrs_mut(&mut self) -> &mut Vec<Attribute> {
        match self {
            Self::Struct(item) => &mut item.attrs,
            Self::Enum(item) => &mut item.attrs,
        }
    }

    /// Returns (field_ident, field_type) pairs for named struct fields.
    pub(crate) fn named_fields(&self) -> Vec<(&Ident, &syn::Type)> {
        match self {
            Self::Struct(item) => item
                .fields
                .iter()
                .filter_map(|f| f.ident.as_ref().map(|id| (id, &f.ty)))
                .collect(),
            Self::Enum(_) => Vec::new(),
        }
    }
}

impl ToTokens for TypeDef {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        match self {
            TypeDef::Struct(item_struct) => item_struct.to_tokens(tokens),
            TypeDef::Enum(item_enum) => item_enum.to_tokens(tokens),
        }
    }
}

pub(crate) struct SubcommandArgs {}

pub(crate) struct ConfigurationArgs {
    pub(crate) key: Option<String>,
    pub(crate) help_heading: Option<String>,
}

pub(crate) struct Subcommand {
    pub(crate) type_def: TypeDef,
    #[allow(dead_code)]
    pub(crate) args: SubcommandArgs,
}

pub(crate) struct Configuration {
    pub(crate) type_def: TypeDef,
    pub(crate) args: ConfigurationArgs,
}

pub(crate) struct ValidatedModule {
    pub(crate) subcommands: Vec<Subcommand>,
    pub(crate) configurations: Vec<Configuration>,
    pub(crate) impls: Vec<ItemImpl>,
    #[allow(dead_code)]
    pub(crate) uses: Vec<syn::ItemUse>,
    pub(crate) module_attributes: Vec<Attribute>,
}

pub(crate) struct ContextArgs {
    pub(crate) context_type_ident: Option<Ident>,
    pub(crate) default_derives: Vec<Path>,
}

impl ContextArgs {
    pub(crate) fn context_type_ident(&self) -> Ident {
        self.context_type_ident
            .clone()
            .unwrap_or_else(|| Ident::new("Context", proc_macro2::Span::call_site()))
    }
}

/// For a configuration type, which subcommands have it as a field and which don't.
pub(crate) struct ConfigMembership<'a> {
    pub(crate) config_ident: &'a Ident,
    pub(crate) present_in: Vec<SubcommandConfigField<'a>>,
    pub(crate) absent_from: Vec<&'a Ident>,
}

pub(crate) struct SubcommandConfigField<'a> {
    pub(crate) subcommand_ident: &'a Ident,
    pub(crate) field_ident: &'a Ident,
}
