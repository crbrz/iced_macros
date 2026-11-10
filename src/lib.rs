use heck::ToUpperCamelCase;
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, Ident, ImplItem, ItemImpl, Pat, ReturnType, Type, parse_macro_input, parse_quote, spanned::Spanned};

/// Use on an `impl` block. Methods marked `#[handler]` become `Message` variants,
/// and an `update` method dispatching to them is generated.
///
/// - 0 arguments: unit variant
/// - 1 argument:  tuple variant
/// - 2+ arguments: struct variant with the original argument names
///
/// Optional: `#[component(MyMessage)]` to override the enum name (default: `Message`).
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let msg_ident: Ident = if attr.is_empty() { format_ident!("Message") } else { parse_macro_input!(attr as Ident) };
    let mut imp = parse_macro_input!(item as ItemImpl);

    match expand(&msg_ident, &mut imp) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(msg: &Ident, imp: &mut ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    let mut variants = Vec::new();
    let mut arms = Vec::new();
    let mut ret: Option<ReturnType> = None;

    for item in imp.items.iter_mut() {
        let ImplItem::Fn(f) = item else { continue };

        // Is it marked #[handler]? If so, remove the marker.
        let before = f.attrs.len();
        f.attrs.retain(|a| !a.path().is_ident("handler"));
        if f.attrs.len() == before {
            continue;
        }

        let fn_ident = f.sig.ident.clone();
        let name = fn_ident.to_string();
        let Some(rest) = name.strip_prefix("handle_") else {
            return Err(syn::Error::new(fn_ident.span(), "handler functions must be named `handle_*`"));
        };
        let variant = format_ident!("{}", rest.to_upper_camel_case());

        // First argument must be a receiver.
        let mut inputs = f.sig.inputs.iter();
        match inputs.next() {
            Some(FnArg::Receiver(_)) => {}
            _ => return Err(syn::Error::new(f.sig.span(), "handler must take `&mut self` as its first argument")),
        }

        // Remaining arguments: collect (name, type).
        let mut args: Vec<(Ident, Type)> = Vec::new();
        for arg in inputs {
            match arg {
                FnArg::Typed(pt) => {
                    let Pat::Ident(pat_ident) = &*pt.pat else {
                        return Err(syn::Error::new(
                            pt.pat.span(),
                            "handler arguments must be plain identifiers (e.g. `value: i32`) \
                             so they can be used as message field names",
                        ));
                    };
                    args.push((pat_ident.ident.clone(), (*pt.ty).clone()));
                }
                FnArg::Receiver(r) => return Err(syn::Error::new(r.span(), "unexpected receiver")),
            }
        }
        let names: Vec<&Ident> = args.iter().map(|(n, _)| n).collect();
        let types: Vec<&Type> = args.iter().map(|(_, t)| t).collect();

        // All handlers must share the same return type; it becomes `update`'s.
        let out = f.sig.output.clone();
        match &ret {
            None => ret = Some(out),
            Some(r) => {
                if quote!(#r).to_string() != quote!(#out).to_string() {
                    return Err(syn::Error::new(out.span(), "all handlers must have the same return type"));
                }
            }
        }

        match args.len() {
            0 => {
                variants.push(quote! { #variant });
                arms.push(quote! { #msg::#variant => self.#fn_ident() });
            }
            1 => {
                variants.push(quote! { #variant(#(#types),*) });
                arms.push(quote! {
                    #msg::#variant(#(#names),*) => self.#fn_ident(#(#names),*)
                });
            }
            _ => {
                variants.push(quote! { #variant { #(#names: #types),* } });
                arms.push(quote! {
                    #msg::#variant { #(#names),* } => self.#fn_ident(#(#names),*)
                });
            }
        }
    }

    let ret = ret.unwrap_or(ReturnType::Default);

    let update: ImplItem = parse_quote! {
        pub fn update(&mut self, message: #msg) #ret {
            match message {
                #(#arms),*
            }
        }
    };
    imp.items.push(update);

    Ok(quote! {
        #[derive(Debug, Clone)]
        pub enum #msg {
            #(#variants),*
        }

        #imp
    })
}
