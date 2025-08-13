use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse::Parse, parse::ParseStream, parse_macro_input, Expr, Fields, Ident, ItemStruct, Lit, Result, Token};
use syn::punctuated::Punctuated;

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
    let input_struct = parse_macro_input!(input as ItemStruct);
    let struct_name = &input_struct.ident;

    // 解析 #[amice(...)]
    let args = parse_macro_input!(args as AmiceArgs);

    // 默认值
    let mut priority_val: i32 = 0;
    let mut name_ts: Option<proc_macro2::TokenStream> = None;
    let mut description_ts: Option<proc_macro2::TokenStream> = None;
    let mut position_ts: Option<proc_macro2::TokenStream> = None;

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
            }
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
            }
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
            }
            "position" => {
                // `PassPosition::PipelineStart | PassPosition::OptimizerLast` 的表达式
                match &kv.value {
                    Expr::Path(_) | Expr::Binary(_) | Expr::Group(_) | Expr::Paren(_) => {
                        let v = &kv.value;
                        position_ts = Some(quote! { #v });
                    }
                    _ => {
                        panic!("#[amice] position 必须是 PassPosition 表达式，例如 `PassPosition::PipelineStart` 或用 `|` 组合");
                    }
                }
            }
            other => {
                panic!("#[amice] 未知参数: {}", other);
            }
        }
    }

    // get_name 默认用结构体名字符串
    let default_name = struct_name.to_string();
    let name_value = name_ts.unwrap_or_else(|| quote! { #default_name });

    // position 默认值
    let position_value = position_ts.expect("Pass必须指定 position");

    // 唯一注册函数名
    let reg_fn_ident = format_ident!("__amice_register__{}", struct_name.to_string().to_lowercase());

    let expanded = quote! {
        #input_struct

        impl crate::pass_registry::AmicePass for #struct_name {
            fn name() -> &'static str {
                #name_value
            }
        }

        #[ctor::ctor]
        fn #reg_fn_ident() {
            fn installer(cfg: &crate::config::Config, manager: &mut llvm_plugin::ModulePassManager, postion: crate::pass_registry::PassPosition) -> bool {
                let allowed_position = #position_value;
                if !allowed_position.contains(postion) {
                    return false;
                }

                let mut pass = #struct_name::default();
                let enabled = <#struct_name as crate::pass_registry::AmicePassLoadable>::init(&mut pass, cfg, postion);
                if !enabled {
                    return false;
                }
                manager.add_pass(pass);

                true
            }

            crate::pass_registry::register(
                crate::pass_registry::PassEntry {
                    name: <#struct_name as crate::pass_registry::AmicePass>::name(),
                    priority: #priority_val,
                    add: installer,
                }
            );
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
        }
        Fields::Unnamed(fields_unnamed) => {
            let calls = (0..fields_unnamed.unnamed.len())
                .map(syn::Index::from)
                .map(|idx| quote! { self.#idx.overlay_env(); });
            quote! { #(#calls)* }
        }
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