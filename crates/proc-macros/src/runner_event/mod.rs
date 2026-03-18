mod codegen;
mod parse;
mod types;
mod validate;

pub(crate) fn handler(input: proc_macro2::TokenStream) -> syn::Result<proc_macro2::TokenStream> {
    let input: types::RunnerEventInput = syn::parse2(input)?;
    validate::validate(&input)?;
    Ok(codegen::codegen(&input))
}
