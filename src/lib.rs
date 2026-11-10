use heck::ToUpperCamelCase;
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, Ident, ImplItem, ItemImpl, ReturnType, parse_macro_input, parse_quote, spanned::Spanned};

/// Use on an `impl` block. Methods marked `#[handler]` become `Message` variants,
/// and an `update` method dispatching to them is generated.
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

        // Remaining arguments become the variant's fields.
        let mut types = Vec::new();
        for arg in inputs {
            match arg {
                FnArg::Typed(pt) => types.push((*pt.ty).clone()),
                FnArg::Receiver(r) => return Err(syn::Error::new(r.span(), "unexpected receiver")),
            }
        }
        let binds: Vec<Ident> = (0..types.len()).map(|i| format_ident!("a{}", i)).collect();

        // All handlers must share the same return type; it becomes `update`'s.
        match &ret {
            None => ret = Some(f.sig.output.clone()),
            Some(r)
                if quote!(#r).to_string() != {
                    let o = &f.sig.output;
                    quote!(#o).to_string()
                } =>
            {
                return Err(syn::Error::new(f.sig.output.span(), "all handlers must have the same return type"));
            }
            _ => {}
        }

        if types.is_empty() {
            variants.push(quote! { #variant });
            arms.push(quote! { #msg::#variant => self.#fn_ident() });
        } else {
            variants.push(quote! { #variant(#(#types),*) });
            arms.push(quote! {
                #msg::#variant(#(#binds),*) => self.#fn_ident(#(#binds),*)
            });
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
