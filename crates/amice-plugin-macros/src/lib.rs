use proc_macro::TokenStream;

use proc_macro2::Span;
use proc_macro2::TokenStream as TokenStream2;

use quote::{format_ident, quote, quote_spanned};

use syn::punctuated::Punctuated;
use syn::{parse::Parse, parse::ParseStream};
use syn::{Error, Expr, Ident, ItemFn, Lit, Token};

struct Kv {
    key: Ident,
    _eq: Token![=],
    value: Expr,
}

impl Parse for Kv {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            key: input.parse()?,
            _eq: input.parse()?,
            value: input.parse()?,
        })
    }
}

struct PluginArgs {
    items: Punctuated<Kv, Token![,]>,
}

impl Parse for PluginArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            items: Punctuated::parse_terminated(input)?,
        })
    }
}

struct ParsedPluginArgs {
    name: String,
    version: String,
    pre_code_gen_callback: Option<Expr>,
}

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
    let args = syn::parse_macro_input::parse::<PluginArgs>(attrs)?;
    let ParsedPluginArgs {
        name,
        version,
        pre_code_gen_callback,
    } = match parse_plugin_args(args) {
        Some(parsed) => parsed?,
        None => return Err(Error::new(Span::call_site(), "`plugin` attr missing args")),
    };

    let func = syn::parse::<ItemFn>(input)?;
    let registrar_name = &func.sig.ident;
    let registrar_name_sys = format_ident!("{}_sys", registrar_name);

    let name = name + "\0";
    let version = version + "\0";
    let pre_code_gen_callback = pre_code_gen_callback
        .map(|callback| quote! { Some(#callback) })
        .unwrap_or_else(|| quote! { None });

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
                #[cfg(feature = "llvm22-1")]
                pre_code_gen_callback: #pre_code_gen_callback,
            }
        }
    })
}

fn parse_plugin_args(args: PluginArgs) -> Option<syn::Result<ParsedPluginArgs>> {
    if args.items.is_empty() {
        return None;
    }

    let mut name = None;
    let mut version = None;
    let mut pre_code_gen_callback = None;

    for arg in args.items {
        match arg.key.to_string().as_str() {
            "name" => match arg.value {
                Expr::Lit(expr_lit) => match expr_lit.lit {
                    Lit::Str(s) => name = Some(s.value()),
                    other => return Some(Err(Error::new_spanned(other, "expected string literal for `name`"))),
                },
                other => return Some(Err(Error::new_spanned(other, "expected string literal for `name`"))),
            },
            "version" => match arg.value {
                Expr::Lit(expr_lit) => match expr_lit.lit {
                    Lit::Str(s) => version = Some(s.value()),
                    other => return Some(Err(Error::new_spanned(other, "expected string literal for `version`"))),
                },
                other => return Some(Err(Error::new_spanned(other, "expected string literal for `version`"))),
            },
            "pre_code_gen_callback" => {
                pre_code_gen_callback = Some(arg.value);
            },
            other => {
                return Some(Err(Error::new_spanned(
                    arg.key,
                    format!("unknown plugin arg `{other}`"),
                )));
            },
        }
    }

    let name = match name {
        Some(name) => name,
        None => return Some(Err(Error::new(Span::call_site(), "`plugin` attr missing `name`"))),
    };
    let version = match version {
        Some(version) => version,
        None => return Some(Err(Error::new(Span::call_site(), "`plugin` attr missing `version`"))),
    };

    Some(Ok(ParsedPluginArgs {
        name,
        version,
        pre_code_gen_callback,
    }))
}
