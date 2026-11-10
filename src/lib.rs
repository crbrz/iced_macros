use heck::ToUpperCamelCase;
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{FnArg, Ident, ImplItem, ItemImpl, Pat, ReturnType, Type, parse_macro_input, parse_quote, spanned::Spanned};

struct Options {
    message: Ident,
    now: bool,
}

/// Use on an `impl` block. Methods marked `#[handler]` become `Message` variants,
/// and an `update` method dispatching to them is generated.
///
/// - 0 fields: unit variant
/// - 1 field:  tuple variant
/// - 2+ fields: struct variant with the original argument names
///
/// Options:
/// - `message = Name`: override the enum name (default: `Message`).
/// - `now`: `update` takes an extra `now: Instant` argument. Handler arguments
///   named `now` are not stored in the message; they receive `update`'s `now`.
///
/// `Instant` is resolved at the call site, so import whichever one you use
/// (`std::time::Instant` or `iced::time::Instant`).
#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut message = format_ident!("Message");
    let mut now = false;

    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("message") {
            message = meta.value()?.parse()?;
            Ok(())
        } else if meta.path.is_ident("now") {
            now = true;
            Ok(())
        } else {
            Err(meta.error("unsupported option; expected `message = Name` or `now`"))
        }
    });
    parse_macro_input!(attr with parser);

    let opts = Options { message, now };
    let mut imp = parse_macro_input!(item as ItemImpl);

    match expand(&opts, &mut imp) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(opts: &Options, imp: &mut ItemImpl) -> syn::Result<proc_macro2::TokenStream> {
    let msg = &opts.message;
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

        // Remaining arguments, in order. `call_args` is everything passed to the
        // handler; `fields` is what ends up in the message (i.e. without `now`).
        let mut call_args: Vec<Ident> = Vec::new();
        let mut fields: Vec<(Ident, Type)> = Vec::new();

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
                    let ident = pat_ident.ident.clone();
                    call_args.push(ident.clone());

                    if ident == "now" {
                        if !opts.now {
                            return Err(syn::Error::new(
                                ident.span(),
                                "this handler takes `now`, but `#[component]` was not given \
                                 the `now` option; use `#[component(now)]`",
                            ));
                        }
                        // Supplied by `update`, not stored in the message.
                    } else {
                        fields.push((ident, (*pt.ty).clone()));
                    }
                }
                FnArg::Receiver(r) => return Err(syn::Error::new(r.span(), "unexpected receiver")),
            }
        }
        let names: Vec<&Ident> = fields.iter().map(|(n, _)| n).collect();
        let types: Vec<&Type> = fields.iter().map(|(_, t)| t).collect();

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

        // Variant shape depends on the fields left after removing `now`.
        match fields.len() {
            0 => {
                variants.push(quote! { #variant });
                arms.push(quote! {
                    #msg::#variant => self.#fn_ident(#(#call_args),*)
                });
            }
            1 => {
                variants.push(quote! { #variant(#(#types),*) });
                arms.push(quote! {
                    #msg::#variant(#(#names),*) => self.#fn_ident(#(#call_args),*)
                });
            }
            _ => {
                variants.push(quote! { #variant { #(#names: #types),* } });
                arms.push(quote! {
                    #msg::#variant { #(#names),* } => self.#fn_ident(#(#call_args),*)
                });
            }
        }
    }

    let ret = ret.unwrap_or(ReturnType::Default);

    let (now_param, now_store) = if opts.now { (quote! { , now: Instant }, quote! { self.now = now; }) } else { (quote! {}, quote! {}) };

    let update: ImplItem = parse_quote! {
        pub fn update(&mut self, message: #msg #now_param) #ret {
            #now_store
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
