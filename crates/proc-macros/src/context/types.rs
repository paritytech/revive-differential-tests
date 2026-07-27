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
        self.attrs()
            .iter()
            .filter(|attr| attr.path().is_ident("doc"))
            .collect()
    }

    pub(crate) fn attrs(&self) -> &[Attribute] {
        match self {
            Self::Struct(item) => &item.attrs,
            Self::Enum(item) => &item.attrs,
        }
    }

    pub(crate) fn attrs_mut(&mut self) -> &mut Vec<Attribute> {
        match self {
            Self::Struct(item) => &mut item.attrs,
            Self::Enum(item) => &mut item.attrs,
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

pub(crate) struct ConfigurationArgs {
    pub(crate) key: Option<String>,
    pub(crate) help_heading: Option<String>,
}

pub(crate) struct Subcommand {
    pub(crate) type_def: TypeDef,
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
    pub(crate) context_type_ident: Ident,
    pub(crate) default_derives: Vec<Path>,
}

impl ContextArgs {
    pub(crate) fn context_type_ident(&self) -> &Ident {
        &self.context_type_ident
    }
}
