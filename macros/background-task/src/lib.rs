extern crate proc_macro;

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{Ident, ItemFn, parse_macro_input};

mod extract_inputs;
mod package_inputs;

#[proc_macro_attribute]
pub fn background_task(_attr: TokenStream, item: TokenStream) -> TokenStream {
    // Parse the input function
    let input_fn = parse_macro_input!(item as ItemFn);
    let input_name = input_fn.sig.ident.clone();
    let input_inputs = input_fn.sig.inputs.clone();

    let name = input_fn.sig.ident.clone().to_string();

    // core function that fires the actual functionality
    let core_name = format!("saps_background_core_{}", name);
    let core_ident = Ident::new(&core_name, Span::call_site());

    // function for registering the function
    let register_name = format!("saps_background_register_{}", name);
    let register_ident = Ident::new(&register_name, Span::call_site());

    // Generate token stream that packages function params into a serde_json::Value
    let package_tokens = package_inputs::generate_package_inputs(&input_inputs);

    let extract_tokens = extract_inputs::generate_extract_inputs(&input_inputs);

    // Extract the body of the original function to inline into the core function
    let fn_body = &input_fn.block.stmts;

    // Generate the expanded code
    let expanded = quote! {

        // function that gets called by the worker
        pub async fn #core_ident (input_val: saps::Value, pool: &'static saps::sqlx::Pool<saps::sqlx::Postgres>) -> Result<(), saps::errors::saps::SapsError> {
            // extract the params from the package
            #extract_tokens
            // original function body
            #(#fn_body)*
            Ok(())
        }

        // interface function that packages params and inserts the task into the DB
        pub async fn #input_name <Z: saps::dal::connections::YieldPostGresPool> (#input_inputs) -> Result<bool, saps::errors::saps::SapsError> {
            use saps::background_tasks::dal::tx_definitions::InsertBackgroundTask;

            // package the inputs as a JSON Value
            #package_tokens

            // create and insert the task into the DB
            let task = saps::background_tasks::dal::model::QueuedTask::new(#name, __saps_params);
            let result = saps::dal::connections::BackgroundTaskPostGresDescriptor::<Z>::insert_background_task(task).await?;
            Ok(result)
        }

        #[ctor::ctor(unsafe)]
        fn #register_ident () {
            saps::background_tasks::registry::TASK_REGISTRY
                .write()
                .unwrap()
                .insert(
                    #name.to_string(),
                    |params, pool| Box::pin(#core_ident(params, pool)),
                );
        }
    };
    TokenStream::from(expanded)
}
