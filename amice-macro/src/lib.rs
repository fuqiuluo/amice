use proc_macro::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Data, DeriveInput, Expr, Fields, Ident, ItemStruct, Lit, Result, Token, Type, parse::Parse, parse::ParseStream,
    parse_macro_input,
};

struct Kv {
    key: Ident,
    _eq: Token![=],
    value: Expr,
}

impl Parse for Kv {
    fn parse(input: ParseStream) -> Result<Self> {
        Ok(Kv {
            key: input.parse()?,
            _eq: input.parse()?,
            value: input.parse()?,
        })
    }
}

// key=value,key=value, ...
struct AmiceArgs {
    items: Punctuated<Kv, Token![,]>,
}

impl Parse for AmiceArgs {
    fn parse(input: ParseStream) -> Result<Self> {
        Ok(AmiceArgs {
            items: Punctuated::parse_terminated(input)?,
        })
    }
}

#[proc_macro_attribute]
pub fn amice(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut input_struct = parse_macro_input!(input as DeriveInput);
    let struct_name = &input_struct.ident;

    // 解析 #[amice(...)]
    let args = parse_macro_input!(args as AmiceArgs);

    // 默认值
    let mut priority_val: i32 = 0;
    let mut name_ts: Option<proc_macro2::TokenStream> = None;
    let mut description_ts: Option<proc_macro2::TokenStream> = None;
    let mut flag_ts: Option<proc_macro2::TokenStream> = None;
    let mut config_ty = None;

    for kv in args.items {
        let key = kv.key.to_string();
        match key.as_str() {
            "priority" => {
                if let Expr::Lit(expr_lit) = &kv.value {
                    if let Lit::Int(li) = &expr_lit.lit {
                        priority_val = li.base10_parse::<i32>().unwrap_or(0);
                    } else {
                        panic!("#[amice] priority 必须是整数字面量");
                    }
                } else {
                    panic!("#[amice] priority 必须是整数字面量");
                }
            },
            "name" => {
                if let Expr::Lit(expr_lit) = &kv.value {
                    if let Lit::Str(ls) = &expr_lit.lit {
                        name_ts = Some(quote! { #ls });
                    } else {
                        panic!("#[amice] name 必须是字符串字面量");
                    }
                } else {
                    panic!("#[amice] name 必须是字符串字面量");
                }
            },
            "description" => {
                if let Expr::Lit(expr_lit) = &kv.value {
                    if let Lit::Str(ls) = &expr_lit.lit {
                        description_ts = Some(quote! { #ls });
                    } else {
                        panic!("#[amice] description 必须是字符串字面量");
                    }
                } else {
                    panic!("#[amice] description 必须是字符串字面量");
                }
            },
            "flag" => {
                // `AmicePassFlag::PipelineStart | AmicePassFlag::OptimizerLast` 的表达式
                match &kv.value {
                    Expr::Path(_) | Expr::Binary(_) | Expr::Group(_) | Expr::Paren(_) => {
                        let v = &kv.value;
                        flag_ts = Some(quote! { #v });
                    },
                    _ => {
                        panic!(
                            "#[amice] flag 必须是 AmicePassFlag 表达式，例如 `AmicePassFlag::PipelineStart` 或用 `|` 组合"
                        );
                    },
                }
            },
            "config" => {
                if let Expr::Path(expr_path) = kv.value {
                    config_ty = Type::Path(syn::TypePath {
                        qself: expr_path.qself,
                        path: expr_path.path,
                    })
                    .into();
                }
            },
            other => {
                panic!("#[amice] 未知参数: {}", other);
            },
        }
    }

    if config_ty.is_none() {
        panic!("#[amice] config 必须提供");
    }

    // get_name 默认用结构体名字符串
    let default_name = struct_name.to_string();
    let name_value = name_ts.unwrap_or_else(|| quote! { #default_name });

    let flag_value = flag_ts.expect("Pass必须指定 flag");

    // 唯一注册函数名
    let reg_fn_ident = format_ident!("__amice_register__{}", struct_name.to_string().to_lowercase());

    let config_ty = config_ty.unwrap();
    if let Data::Struct(ref mut data_struct) = input_struct.data {
        if let Fields::Named(ref mut fields) = data_struct.fields {
            let common_fields: Vec<syn::Field> = vec![
                syn::parse_quote! { pub default_config: #config_ty }, // 从环境变量或者配置文件获取的默认参数
            ];

            for field in common_fields {
                fields.named.push(field);
            }
        }
    }

    let input_struct = if flag_value.to_string().contains("FunctionLevel") {
        quote! {
            #input_struct

            impl crate::pass_registry::AmiceFunctionPass for #struct_name {
                type Config = #config_ty;

                fn parse_function_annotations<'a>(&self, module: &mut llvm_plugin::inkwell::module::Module<'a>, function: llvm_plugin::inkwell::values::FunctionValue<'a>) -> anyhow::Result<#config_ty> {
                    let def_cfg = &self.default_config;
                    let cfg = <#config_ty as crate::pass_registry::FunctionAnnotationsOverlay>::overlay_annotations(def_cfg, module, function);
                    cfg
                }
            }
        }
    } else {
        input_struct.to_token_stream()
    };

    let expanded = quote! {
        #input_struct

        impl crate::pass_registry::AmicePassMetadata for #struct_name {
            fn name() -> &'static str {
                #name_value
            }

            fn flag() -> crate::pass_registry::AmicePassFlag {
                #flag_value
            }
        }

        impl llvm_plugin::LlvmModulePass for #struct_name {
            fn run_pass(&self, module: &mut llvm_plugin::inkwell::module::Module<'_>, _manager: &llvm_plugin::ModuleAnalysisManager) -> llvm_plugin::PreservedAnalyses {
                let name = <#struct_name as crate::pass_registry::AmicePassMetadata>::name();
                let flag = <#struct_name as crate::pass_registry::AmicePassMetadata>::flag();

                let result = match self.do_pass(module) {
                    Ok(analyses) => analyses,
                    Err(e) => {
                        log::error!("({}) do_pass failed: {}", name, e);
                        llvm_plugin::PreservedAnalyses::All
                    }
                };

                match result {
                    llvm_plugin::PreservedAnalyses::None => log::info!("({}) pass done", name),
                    _ => {}
                };

                result
            }
        }

        #[ctor::ctor]
        fn #reg_fn_ident() {
            fn installer(cfg: &crate::config::Config, manager: &mut llvm_plugin::ModulePassManager, flag: crate::pass_registry::AmicePassFlag) -> bool {
                let allowed_flag = #flag_value;
                if !allowed_flag.contains(flag) {
                    return false;
                }

                let mut pass = #struct_name::default();
                <#struct_name as crate::pass_registry::AmicePass>::init(&mut pass, cfg, flag);
                manager.add_pass(pass);

                true
            }

            crate::pass_registry::register(
                crate::pass_registry::PassEntry {
                    name: <#struct_name as crate::pass_registry::AmicePassMetadata>::name(),
                    priority: #priority_val,
                    add: installer,
                }
            );
        }

        #[allow(unused_macros)]
        macro_rules! error {
            ($($arg:tt)+) => (log::error!("({}) {}", #name_value, format!($($arg)+)))
        }
        #[allow(unused_macros)]
        macro_rules! warn {
            ($($arg:tt)+) => (log::warn!("({}) {}", #name_value, format!($($arg)+)))
        }
        #[allow(unused_macros)]
        macro_rules! info {
            ($($arg:tt)+) => (log::info!("({}) {}", #name_value, format!($($arg)+)))
        }
        #[allow(unused_macros)]
        macro_rules! debug {
            ($($arg:tt)+) => (log::debug!("({}) {}", #name_value, format!($($arg)+)))
        }
        #[allow(unused_macros)]
        macro_rules! trace {
            ($($arg:tt)+) => (log::trace!("({}) {}", #name_value, format!($($arg)+)))
        }
    };

    TokenStream::from(expanded)
}

#[proc_macro_attribute]
pub fn amice_config_manager(_args: TokenStream, input: TokenStream) -> TokenStream {
    let input_struct = parse_macro_input!(input as ItemStruct);
    let struct_name = &input_struct.ident;

    // 仅生成“管理器”实现：遍历字段，逐个调用 overlay_env()
    let overlay_body = match &input_struct.fields {
        Fields::Named(fields_named) => {
            let calls = fields_named.named.iter().filter_map(|f| f.ident.as_ref()).map(|ident| {
                quote! { self.#ident.overlay_env(); }
            });
            quote! { #(#calls)* }
        },
        Fields::Unnamed(fields_unnamed) => {
            let calls = (0..fields_unnamed.unnamed.len())
                .map(syn::Index::from)
                .map(|idx| quote! { self.#idx.overlay_env(); });
            quote! { #(#calls)* }
        },
        Fields::Unit => quote! {},
    };

    let expanded = quote! {
        #input_struct

        impl crate::pass_registry::EnvOverlay for #struct_name {
            fn overlay_env(&mut self) {
                #overlay_body
            }
        }
    };

    TokenStream::from(expanded)
}
