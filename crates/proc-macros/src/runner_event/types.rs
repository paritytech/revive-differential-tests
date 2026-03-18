use proc_macro2::Ident;
use syn::{Attribute, Type, Visibility};

pub(crate) struct RunnerEventInput {
    pub attrs: Vec<Attribute>,
    pub vis: Visibility,
    pub enum_ident: Ident,
    pub groups: Vec<EventGroup>,
}

pub(crate) struct EventGroup {
    pub key: Ident,
    pub events: Vec<EventDef>,
}

pub(crate) struct EventDef {
    pub attrs: Vec<Attribute>,
    pub ident: Ident,
    pub fields: Vec<EventField>,
}

pub(crate) struct EventField {
    pub attrs: Vec<Attribute>,
    pub ident: Ident,
    pub ty: Type,
}
