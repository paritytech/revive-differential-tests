mod context;

#[proc_macro_attribute]
pub fn context(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let item = syn::parse_macro_input!(item as syn::ItemMod);
    match context::handler(attr.into(), item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
