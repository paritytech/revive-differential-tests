use std::collections::{HashMap, HashSet};

use super::codegen::to_snake_case;
use super::types::RunnerEventInput;

pub(crate) fn validate(input: &RunnerEventInput) -> syn::Result<()> {
    // Reject duplicate group keys.
    let mut seen_keys = HashSet::new();
    for group in &input.groups {
        let key_str = group.key.to_string();
        if !seen_keys.insert(key_str.clone()) {
            return Err(syn::Error::new(
                group.key.span(),
                format!("duplicate group key `{key_str}`"),
            ));
        }
    }

    // Reject duplicate event names across all groups.
    let mut seen_events = HashMap::new();
    for group in &input.groups {
        for event in &group.events {
            let name = event.ident.to_string();
            if let Some(prev_span) = seen_events.insert(name.clone(), event.ident.span()) {
                let mut err =
                    syn::Error::new(event.ident.span(), format!("duplicate event name `{name}`"));
                err.combine(syn::Error::new(prev_span, "first defined here"));
                return Err(err);
            }
        }
    }

    // Reject events with fields named the same as the auto-generated specifier field.
    for group in &input.groups {
        if group.key == "Reporter" {
            continue;
        }
        let specifier_field = to_snake_case(&group.key.to_string());
        for event in &group.events {
            for field in &event.fields {
                if field.ident == specifier_field.as_str() {
                    return Err(syn::Error::new(
                        field.ident.span(),
                        format!(
                            "field `{}` conflicts with the auto-generated specifier field",
                            field.ident
                        ),
                    ));
                }
            }
        }
    }

    Ok(())
}
