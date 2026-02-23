use proc_macro2::TokenStream;
use quote::quote;
use syn::ItemMod;

use super::codegen_as_ref::{
    compute_config_membership, gen_context_as_ref_impls, gen_default_statics,
    gen_subcommand_as_ref_impls,
};
use super::types::{ContextArgs, ValidatedModule};
use super::validate::validate;

pub(crate) fn handler(attr: TokenStream, module: ItemMod) -> syn::Result<TokenStream> {
    let args: ContextArgs = syn::parse2(attr)?;
    let ValidatedModule {
        subcommands,
        configurations,
        impls,
        uses: _,
        module_attributes,
    } = validate(module, &args)?;

    let context_type_ident = args.context_type_ident();

    // Generate the context enum definition with boxed subcommand variants.
    let context_type_definition = {
        let subcommand_type_idents = subcommands.iter().map(|item| item.type_def.ident());
        let subcommand_doc_attrs = subcommands.iter().map(|item| item.type_def.doc_attrs());
        let default_derives = &args.default_derives;
        quote! {
            #[derive(#(#default_derives),*)]
            #(#module_attributes)*
            pub enum #context_type_ident {
                #(
                    #(#subcommand_doc_attrs)*
                    #subcommand_type_idents(std::boxed::Box<#subcommand_type_idents>)
                ),*
            }
        }
    };

    // Type definitions for subcommands and configurations.
    let subcommand_type_definitions = subcommands
        .iter()
        .map(|item| &item.type_def)
        .collect::<Vec<_>>();
    let configuration_type_definitions = configurations
        .iter()
        .map(|item| &item.type_def)
        .collect::<Vec<_>>();

    // Compute config membership (which subcommands have which configs).
    let memberships = compute_config_membership(&subcommands, &configurations)?;

    // Generate default statics for configs missing from some subcommands.
    let default_statics = gen_default_statics(&memberships);

    // Generate AsRef impls on subcommands.
    let subcommand_as_ref_impls = gen_subcommand_as_ref_impls(&memberships);

    // Generate AsRef impls on the context enum.
    let context_as_ref_impls =
        gen_context_as_ref_impls(&context_type_ident, &subcommands, &memberships);

    // Collect just the idents for IsConfig assertions.
    let configuration_type_idents = configurations
        .iter()
        .map(|item| item.type_def.ident())
        .collect::<Vec<_>>();

    // Compile-time assertions that all subcommand fields are configuration types.
    let subcommand_field_assertions = subcommands.iter().flat_map(|item| {
        item.type_def.field_types().into_iter().map(|ty| {
            quote! {
                const _: fn() = || {
                    fn assert_is_config<T: IsConfig>() {}
                    assert_is_config::<#ty>();
                };
            }
        })
    });

    Ok(quote! {
        // 1. Context enum
        #context_type_definition

        // 2. Subcommand type definitions
        #(#subcommand_type_definitions)*

        // 3. Configuration type definitions
        #(#configuration_type_definitions)*

        // 4. User-defined impl blocks
        #(#impls)*

        // 5. All macro-generated impls, statics, and assertions live in a const block
        //    so that their context is separate from user code.
        const _: () = {
            // Default statics for configs missing from some subcommands
            #default_statics

            // AsRef on subcommands (ALL subcommands × ALL configs)
            #subcommand_as_ref_impls

            // AsRef on context enum (pure delegation)
            #context_as_ref_impls

            /// A trait which we implement on all configuration items to ensure that only configs
            /// are used as fields in the subcommand struct fields.
            trait IsConfig {};

            #(
                impl IsConfig for #configuration_type_idents {}
            )*

            #(#subcommand_field_assertions)*
        };
    })
}
