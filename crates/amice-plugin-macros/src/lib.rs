use proc_macro::TokenStream;

use proc_macro2::Span;
use proc_macro2::TokenStream as TokenStream2;

use quote::{format_ident, quote, quote_spanned};

use syn::ItemFn;
use syn::{AttributeArgs, Error};

/// Macro for defining a new LLVM plugin.
///
/// This macro must be used on a function, and needs a `name` and `version`
/// parameters.
///
/// The annotated function will be used as the plugin's entrypoint, and must
/// take a `PassBuilder` as argument.
///
/// # Warning
///
/// This macro should be used on `cdylib` crates **only**. Also, since it generates
/// an export symbol, it should be used **once** for the whole dylib being compiled.
///
/// # Example
///
/// ```ignore
/// # use amice_plugin::PassBuilder;
/// #[llvm_plugin::plugin(name = "plugin_name", version = "0.1")]
/// fn plugin_registrar(builder: &mut PassBuilder) {
///     builder.add_module_pipeline_parsing_callback(|name, pass_manager| {
///         // add passes to the pass manager
///         # todo!()
///     });
///
///     builder.add_module_analysis_registration_callback(|analysis_manager| {
///         // register analyses to the analysis manager
///         # todo!()
///     });
/// }
/// ```
#[proc_macro_attribute]
pub fn plugin(attrs: TokenStream, input: TokenStream) -> TokenStream {
    match plugin_impl(attrs, input) {
        Ok(ts) => ts.into(),
        Err(e) => {
            let msg = e.to_string();
            quote_spanned! { e.span() => fn error() { std::compile_error!(#msg) } }.into()
        },
    }
}

fn plugin_impl(attrs: TokenStream, input: TokenStream) -> syn::Result<TokenStream2> {
    let args = syn::parse_macro_input::parse(attrs)?;
    let (name, version) = match parse_plugin_args(args) {
        Some(parsed) => parsed?,
        None => return Err(Error::new(Span::call_site(), "`plugin` attr missing args")),
    };

    let func = syn::parse::<ItemFn>(input)?;
    let registrar_name = &func.sig.ident;
    let registrar_name_sys = format_ident!("{}_sys", registrar_name);

    let name = name + "\0";
    let version = version + "\0";

    Ok(quote! {
        #func

        extern "C" fn #registrar_name_sys(builder: *mut std::ffi::c_void) {
            let mut builder = unsafe { amice_plugin::PassBuilder::from_raw(builder) };
            #registrar_name(&mut builder);
        }

        #[no_mangle]
        extern "C" fn llvmGetPassPluginInfo() -> amice_plugin::PassPluginLibraryInfo {
            amice_plugin::PassPluginLibraryInfo {
                api_version: amice_plugin::get_llvm_plugin_api_version__(),
                plugin_name: #name.as_ptr(),
                plugin_version: #version.as_ptr(),
                plugin_registrar: #registrar_name_sys,
            }
        }
    })
}

fn parse_plugin_args(args: AttributeArgs) -> Option<syn::Result<(String, String)>> {
    let mut args_iter = args.iter();

    let arg = args_iter.next()?;
    let name = match arg {
        syn::NestedMeta::Meta(syn::Meta::NameValue(syn::MetaNameValue {
            path,
            lit: syn::Lit::Str(s),
            ..
        })) if path.is_ident("name") => s.value(),
        _ => {
            return Some(Err(Error::new_spanned(arg, "expected arg `name=\"value\"`")));
        },
    };

    let arg = args_iter.next()?;
    let version = match arg {
        syn::NestedMeta::Meta(syn::Meta::NameValue(syn::MetaNameValue {
            path,
            lit: syn::Lit::Str(s),
            ..
        })) if path.is_ident("version") => s.value(),
        _ => {
            return Some(Err(Error::new_spanned(arg, "expected arg `version=\"value\"`")));
        },
    };

    Some(Ok((name, version)))
}
