//! Proc macros for kernel spike B (`kernel-design-b-traitenv.md` §2.1, §3).
//!
//! - `#[derive(Env)]`: `Env` + `Has<F>`/`Replace<F>` for every field type, duplicate detection,
//!   `#[env(flatten)]` through a derive-emitted `macro_rules!` forwarder, and
//!   `#[env(base(forward(T, ..)))]` for a generic base env.
//! - `effect!` / `effect_local!`: `|r| body` closures where `~Cap` reads a capability
//!   (`<_ as Has<Cap>>::get(r).clone()`) and `~ expr` binds an effect (same `R`, or env-free `()`).

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Group, Ident, Spacing, Span, TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote};
use syn::parse::Parse;
use syn::punctuated::Punctuated;
use syn::{Data, DeriveInput, Fields, Token, Type, parse_macro_input};

// ── #[derive(Env)] ─────────────────────────────────────────────────────────────────────────

enum FieldKind {
  Own,
  Flatten,
  Base(Vec<Type>),
}

fn field_kind(field: &syn::Field) -> syn::Result<FieldKind> {
  let mut kind = FieldKind::Own;
  for attr in &field.attrs {
    if !attr.path().is_ident("env") {
      continue;
    }
    attr.parse_nested_meta(|meta| {
      if meta.path.is_ident("flatten") {
        kind = FieldKind::Flatten;
        Ok(())
      } else if meta.path.is_ident("base") {
        let mut fwd = Vec::new();
        meta.parse_nested_meta(|inner| {
          if inner.path.is_ident("forward") {
            let content;
            syn::parenthesized!(content in inner.input);
            let list: Punctuated<Type, Token![,]> =
              content.parse_terminated(Type::parse, Token![,])?;
            fwd.extend(list);
            Ok(())
          } else {
            Err(inner.error("expected `forward(..)`"))
          }
        })?;
        kind = FieldKind::Base(fwd);
        Ok(())
      } else {
        Err(meta.error("expected `flatten` or `base(forward(..))`"))
      }
    })?;
  }
  Ok(kind)
}

fn type_key(ty: &Type) -> String {
  quote!(#ty).to_string().replace(' ', "")
}

fn last_ident(ty: &Type) -> syn::Result<Ident> {
  if let Type::Path(p) = ty
    && let Some(seg) = p.path.segments.last()
  {
    if !seg.arguments.is_empty() {
      return Err(syn::Error::new_spanned(
        ty,
        "#[env(flatten)] supports only non-generic env structs in the spike",
      ));
    }
    return Ok(seg.ident.clone());
  }
  Err(syn::Error::new_spanned(
    ty,
    "#[env(flatten)] needs a path to a #[derive(Env)] struct",
  ))
}

/// `#[derive(Env)]`: see the crate docs.
#[proc_macro_derive(Env, attributes(env))]
pub fn derive_env(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  match expand_env(&input) {
    Ok(ts) => ts.into(),
    Err(e) => e.to_compile_error().into(),
  }
}

fn expand_env(input: &DeriveInput) -> syn::Result<TokenStream2> {
  let name = &input.ident;
  let Data::Struct(data) = &input.data else {
    return Err(syn::Error::new_spanned(
      name,
      "#[derive(Env)] works on structs only",
    ));
  };
  let Fields::Named(fields) = &data.fields else {
    return Err(syn::Error::new_spanned(
      name,
      "#[derive(Env)] needs named fields",
    ));
  };

  let mut generics = input.generics.clone();
  for tp in generics.type_params_mut() {
    tp.bounds.push(syn::parse_quote!(::core::clone::Clone));
    tp.bounds.push(syn::parse_quote!(::core::marker::Send));
    tp.bounds.push(syn::parse_quote!(::core::marker::Sync));
    tp.bounds.push(syn::parse_quote!('static));
  }
  let (impl_g, ty_g, where_c) = generics.split_for_impl();
  let is_generic = !input.generics.params.is_empty();

  // Duplicate detection over every type this struct provides directly (own fields + forward
  // lists). Duplicates arriving through `flatten` can only surface as rustc's E0119.
  let mut seen: Vec<(String, String)> = Vec::new();
  let mut out = TokenStream2::new();
  let mut fwd_own = TokenStream2::new();
  let mut fwd_nested = TokenStream2::new();
  let mut flatten_calls = TokenStream2::new();

  let where_preds: Vec<_> = where_c
    .map(|w| w.predicates.iter().cloned().collect())
    .unwrap_or_default();

  for field in &fields.named {
    let fname = field.ident.as_ref().expect("named field");
    let fty = &field.ty;
    let kind = field_kind(field)?;
    let mut provide = |ty: &Type, via: &str| -> syn::Result<()> {
      let key = type_key(ty);
      if let Some((_, other)) = seen.iter().find(|(k, _)| *k == key) {
        return Err(syn::Error::new_spanned(
          ty,
          format!(
            "`{key}` is provided twice in `{name}` (fields `{other}`, `{via}`)\n\
             help: wrap one in a newtype (`struct Replica{key}({key});`)"
          ),
        ));
      }
      seen.push((key, via.to_string()));
      Ok(())
    };
    match kind {
      FieldKind::Own => {
        provide(fty, &fname.to_string())?;
        out.extend(quote! {
          impl #impl_g ::kernel_b::Has<#fty> for #name #ty_g #where_c {
            #[inline]
            fn get(&self) -> &#fty { &self.#fname }
          }
          impl #impl_g ::kernel_b::Replace<#fty> for #name #ty_g #where_c {
            #[inline]
            fn replace(&mut self, value: #fty) -> #fty { ::core::mem::replace(&mut self.#fname, value) }
          }
        });
        fwd_own.extend(quote! {
          impl<$($g)*> ::kernel_b::Has<#fty> for $target where $($w)* {
            #[inline]
            fn get(&self) -> &#fty { &self.$($path)+ . #fname }
          }
          impl<$($g)*> ::kernel_b::Replace<#fty> for $target where $($w)* {
            #[inline]
            fn replace(&mut self, value: #fty) -> #fty {
              ::core::mem::replace(&mut self.$($path)+ . #fname, value)
            }
          }
        });
      }
      FieldKind::Base(list) => {
        for t in &list {
          provide(t, &format!("{fname} (forward)"))?;
          let mut has_preds = where_preds.clone();
          has_preds.push(syn::parse_quote!(#fty: ::kernel_b::Has<#t>));
          let mut preds = where_preds.clone();
          preds.push(syn::parse_quote!(#fty: ::kernel_b::Replace<#t>));
          out.extend(quote! {
            impl #impl_g ::kernel_b::Has<#t> for #name #ty_g where #(#has_preds,)* {
              #[inline]
              fn get(&self) -> &#t { ::kernel_b::Has::<#t>::get(&self.#fname) }
            }
            impl #impl_g ::kernel_b::Replace<#t> for #name #ty_g where #(#preds,)* {
              #[inline]
              fn replace(&mut self, value: #t) -> #t {
                ::kernel_b::Replace::<#t>::replace(&mut self.#fname, value)
              }
            }
          });
        }
      }
      FieldKind::Flatten => {
        let inner = last_ident(fty)?;
        let mac = format_ident!("__kernel_b_forward_{}", inner);
        let g_params = &generics.params;
        flatten_calls.extend(quote! {
          #mac!([#g_params] [#(#where_preds,)*] #name #ty_g, #fname);
        });
        fwd_nested.extend(quote! {
          #mac!([$($g)*] [$($w)*] $target, $($path)+ . #fname);
        });
      }
    }
  }

  out.extend(quote! {
    impl #impl_g ::kernel_b::Env for #name #ty_g #where_c {}
  });
  out.extend(flatten_calls);

  if !is_generic {
    let mac = format_ident!("__kernel_b_forward_{}", name);
    out.extend(quote! {
      #[doc(hidden)]
      #[allow(unused_macros)]
      macro_rules! #mac {
        ([$($g:tt)*] [$($w:tt)*] $target:ty, $($path:tt)+) => {
          #fwd_own
          #fwd_nested
        };
      }
    });
  }
  Ok(out)
}

// ── effect! / effect_local! ─────────────────────────────────────────────────────────────────

/// `effect!(|r| body)`: a `Send` effect whose body is an `async` block over `r: &mut R`.
#[proc_macro]
pub fn effect(input: TokenStream) -> TokenStream {
  expand_effect(input.into(), false)
    .unwrap_or_else(|e| e.to_compile_error())
    .into()
}

/// `effect_local!(|r| body)`: the same, producing a `!Send`-capable `LocalEffect`.
#[proc_macro]
pub fn effect_local(input: TokenStream) -> TokenStream {
  expand_effect(input.into(), true)
    .unwrap_or_else(|e| e.to_compile_error())
    .into()
}

fn is_punct(tt: Option<&TokenTree>, ch: char) -> bool {
  matches!(tt, Some(TokenTree::Punct(p)) if p.as_char() == ch)
}

fn expand_effect(input: TokenStream2, local: bool) -> syn::Result<TokenStream2> {
  let toks: Vec<TokenTree> = input.into_iter().collect();
  let mut i = 0;
  if matches!(toks.first(), Some(TokenTree::Ident(id)) if id == "move") {
    i += 1;
  }
  if !is_punct(toks.get(i), '|') {
    return Err(syn::Error::new(
      Span::call_site(),
      "effect! expects a closure: `effect!(|r| body)` or `effect!(|r: &mut R| body)`",
    ));
  }
  i += 1;
  let mut param = Vec::new();
  while i < toks.len() && !is_punct(toks.get(i), '|') {
    param.push(toks[i].clone());
    i += 1;
  }
  if i >= toks.len() {
    return Err(syn::Error::new(
      Span::call_site(),
      "effect!: unterminated closure parameter",
    ));
  }
  i += 1;
  let Some(TokenTree::Ident(r)) = param.first().cloned() else {
    return Err(syn::Error::new(
      Span::call_site(),
      "effect!: the closure parameter must be an identifier",
    ));
  };
  let body: TokenStream2 = rewrite(&toks[i..], &r);
  let param: TokenStream2 = param.into_iter().collect();
  Ok(if local {
    quote! {
      ::kernel_b::LocalEffect::from_fn_async(move |#param| ::kernel_b::box_local_future(async move {
        let _ = &#r;
        ::core::result::Result::Ok({ #body })
      }))
    }
  } else {
    quote! {
      ::kernel_b::Effect::from_fn_async(move |#param| ::kernel_b::box_future(async move {
        let _ = &#r;
        ::core::result::Result::Ok({ #body })
      }))
    }
  })
}

fn starts_upper(id: &Ident) -> bool {
  id.to_string()
    .chars()
    .next()
    .is_some_and(char::is_uppercase)
}

/// Rewrite `~Cap` and `~ expr` occurrences in a token sequence (recursing into groups).
fn rewrite(toks: &[TokenTree], r: &Ident) -> TokenStream2 {
  let mut out = TokenStream2::new();
  let mut i = 0;
  while i < toks.len() {
    match &toks[i] {
      TokenTree::Punct(p) if p.as_char() == '~' => {
        i += 1;
        let (consumed, ts) = rewrite_tilde(&toks[i..], r);
        out.extend(ts);
        i += consumed;
      }
      // A nested `effect!(..)` / `effect_local!(..)` rewrites its own body with its own `r`.
      TokenTree::Ident(id)
        if (id == "effect" || id == "effect_local")
          && is_punct(toks.get(i + 1), '!')
          && matches!(toks.get(i + 2), Some(TokenTree::Group(_))) =>
      {
        out.extend(toks[i..i + 3].iter().cloned());
        i += 3;
      }
      TokenTree::Group(g) => {
        let inner: Vec<TokenTree> = g.stream().into_iter().collect();
        let mut ng = Group::new(g.delimiter(), rewrite(&inner, r));
        ng.set_span(g.span());
        out.extend([TokenTree::Group(ng)]);
        i += 1;
      }
      other => {
        out.extend([other.clone()]);
        i += 1;
      }
    }
  }
  out
}

/// Parse a path `a::b::C` (no generics); returns the number of tokens.
fn path_len(toks: &[TokenTree]) -> usize {
  let mut i = 0;
  if !matches!(toks.first(), Some(TokenTree::Ident(_))) {
    return 0;
  }
  i += 1;
  while is_punct(toks.get(i), ':')
    && is_punct(toks.get(i + 1), ':')
    && matches!(toks.get(i + 2), Some(TokenTree::Ident(_)))
  {
    i += 3;
  }
  i
}

fn rewrite_tilde(toks: &[TokenTree], r: &Ident) -> (usize, TokenStream2) {
  let plen = path_len(toks);
  if plen > 0 {
    let last = match &toks[plen - 1] {
      TokenTree::Ident(id) => id.clone(),
      _ => unreachable!(),
    };
    let next = toks.get(plen);
    let call_like = matches!(next, Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis || g.delimiter() == Delimiter::Brace)
      || is_punct(next, '!')
      || (is_punct(next, ':') && is_punct(toks.get(plen + 1), ':'));
    if starts_upper(&last) && !call_like {
      let path: TokenStream2 = toks[..plen].iter().cloned().collect();
      return (
        plen,
        quote! { (::core::clone::Clone::clone(::kernel_b::Has::<#path>::get(&*#r))) },
      );
    }
  }
  // Bind: a postfix chain `path(::<..>)?((..)|[..]|.ident|.0)*`, or a parenthesised group.
  let mut i = 0;
  if plen > 0 {
    i = plen;
    // `path!(..)`: a macro producing an effect (e.g. a nested `effect!`).
    if is_punct(toks.get(i), '!') && matches!(toks.get(i + 1), Some(TokenTree::Group(_))) {
      i += 2;
    }
  } else if matches!(toks.first(), Some(TokenTree::Group(_))) {
    i = 1;
  }
  loop {
    match toks.get(i) {
      Some(TokenTree::Group(g)) if g.delimiter() != Delimiter::Brace => i += 1,
      Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint => {
        // `::<..>` turbofish or `::segment`
        if is_punct(toks.get(i + 1), ':') && is_punct(toks.get(i + 2), '<') {
          let mut depth = 0i32;
          let mut j = i + 2;
          while let Some(t) = toks.get(j) {
            if is_punct(Some(t), '<') {
              depth += 1;
            } else if is_punct(Some(t), '>') {
              depth -= 1;
              if depth == 0 {
                break;
              }
            }
            j += 1;
          }
          i = j + 1;
        } else if is_punct(toks.get(i + 1), ':')
          && matches!(toks.get(i + 2), Some(TokenTree::Ident(_)))
        {
          i += 3;
        } else {
          break;
        }
      }
      Some(TokenTree::Punct(p)) if p.as_char() == '.' => match toks.get(i + 1) {
        Some(TokenTree::Ident(id)) if id != "await" => i += 2,
        Some(TokenTree::Literal(_)) => i += 2,
        _ => break,
      },
      _ => break,
    }
  }
  let chain = rewrite(&toks[..i], r);
  (
    i,
    quote! {
      (match {
        let __kb_x = #chain;
        let __kb_tag = ::kernel_b::__widen::ErrTag::__kb_err_tag(&__kb_x);
        (::kernel_b::__bind(__kb_x, &mut *#r).await, __kb_tag)
      } {
        (::core::result::Result::Ok(v), __kb_tag) => ::kernel_b::__widen::Tag::out(&__kb_tag, v),
        (::core::result::Result::Err(c), __kb_tag) => return ::core::result::Result::Err({
          #[allow(unused_imports)]
          use ::kernel_b::__widen::{ViaInto as _, ViaNever as _};
          (&&__kb_tag).__kb_widen(c)
        }),
      })
    },
  )
}
