use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, Pat, punctuated::Punctuated, token::Comma};

/// Generates a `TokenStream` that packages all typed function parameters into
/// a `saps::Value::Object(map)` stored in a variable called `__saps_params`.
///
/// For a function with signature `fn foo(user_id: i32, name: String)`, this produces:
///
/// ```ignore
/// let mut __saps_map = serde_json::Map::new();
/// __saps_map.insert("user_id".to_string(), serde_json::to_value(&user_id).expect("..."));
/// __saps_map.insert("name".to_string(), serde_json::to_value(&name).expect("..."));
/// let __saps_params = saps::Value::Object(__saps_map);
/// ```
pub fn generate_package_inputs(inputs: &Punctuated<FnArg, Comma>) -> TokenStream {
    // define the buffers
    let mut param_idents = Vec::new();
    let mut param_names = Vec::new();

    // loop through the inputs and package them to the buffers
    for arg in inputs {
        // only package if a typed input
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                let ident = &pat_ident.ident;
                param_names.push(ident.to_string());
                param_idents.push(ident.clone());
            }
        }
    }

    quote! {
        let mut __saps_map = serde_json::Map::new();
        #(
            __saps_map.insert(
                #param_names.to_string(),
                serde_json::to_value(&#param_idents).expect(
                    &format!("failed to serialize parameter '{}' to JSON", #param_names)
                ),
            );
        )*
        let __saps_params = saps::Value::Object(__saps_map);
    }
}
