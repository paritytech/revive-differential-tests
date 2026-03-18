use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Attribute, Ident, Token, Type, Visibility, braced};

use super::types::{EventDef, EventField, EventGroup, RunnerEventInput};

impl Parse for RunnerEventInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let vis: Visibility = input.parse()?;
        input.parse::<Token![enum]>()?;
        let enum_ident: Ident = input.parse()?;

        let content;
        braced!(content in input);

        let mut groups = Vec::new();
        while !content.is_empty() {
            groups.push(content.parse::<EventGroup>()?);
            if !content.peek(Token![,]) {
                break;
            }
            content.parse::<Token![,]>()?;
        }

        Ok(RunnerEventInput {
            attrs,
            vis,
            enum_ident,
            groups,
        })
    }
}

impl Parse for EventGroup {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        input.parse::<Token![=>]>()?;

        let content;
        braced!(content in input);

        let mut events = Vec::new();
        while !content.is_empty() {
            events.push(content.parse::<EventDef>()?);
            if !content.peek(Token![,]) {
                break;
            }
            content.parse::<Token![,]>()?;
        }

        Ok(EventGroup { key, events })
    }
}

impl Parse for EventDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let ident: Ident = input.parse()?;

        let content;
        braced!(content in input);

        let fields = Punctuated::<EventField, Token![,]>::parse_terminated(&content)?;
        let fields: Vec<EventField> = fields.into_iter().collect();

        Ok(EventDef {
            attrs,
            ident,
            fields,
        })
    }
}

impl Parse for EventField {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let ident: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let ty: Type = input.parse()?;

        Ok(EventField { attrs, ident, ty })
    }
}
