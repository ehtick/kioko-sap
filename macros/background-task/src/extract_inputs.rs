use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, Pat, punctuated::Punctuated, token::Comma};

pub fn generate_extract_inputs(inputs: &Punctuated<FnArg, Comma>) -> TokenStream {
    let mut param_names = Vec::new();
    let mut param_idents = Vec::new();
    let mut param_types = Vec::new();

    for arg in inputs {
        // only package if a typed input
        if let FnArg::Typed(pat_type) = arg {
            if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
                let ident = &pat_ident.ident;
                param_names.push(ident.to_string());
                param_idents.push(ident.clone());
                param_types.push(pat_type.ty.clone());
            }
        }
    }

    quote! {
        #(
            let #param_idents: #param_types = serde_json::from_value(
                input_val.get(#param_names)
                    .expect(&format!("missing parameter '{}'", #param_names))
                    .clone()
            ).expect(&format!("failed to deserialize parameter '{}'", #param_names));
        )*
    }
}
