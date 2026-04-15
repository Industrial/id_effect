use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Result, Token, Type};

/// Parsed surface syntax for the `effect!` procedural macro.
///
/// Supported: do-notation with `|env: &mut R| { ... }`, optional `move`, or bare `effect! { ... }`.
///
/// Expansion: bind-free bodies with **no** `.await` use `Effect::new` (sync closure). Bodies **with**
/// `~` or **with** `.await` use `Effect::new_async` and an inner `async move` block. You do not write
/// `async` on the closure — the macro wraps the body when needed.
pub enum EffectKind {
  /// `effect!(|env: &mut R| { ... })` or `effect!(move |env: &mut R| { ... })` — do-notation; outer closure is always `move` in the expansion.
  DoNotation {
    param: Ident,
    env_ty: Box<Type>,
    body: TokenStream,
  },
  /// `effect! { ... }` — do-notation with implicit `__effect_r: &mut ()`.
  Bare { body: TokenStream },
}

const ASYNC_CLOSURE_UNSUPPORTED: &str = "effect!: `async |...|` is not supported — effects are already async. \
Use `|env: &mut R| { ... }` for do-notation.";

const ASYNC_BLOCK_UNSUPPORTED: &str = "effect!: `async { ... }` is not supported — effects are already async. \
Use `|_: &mut ()| { ... }` or `effect! { ... }` when the environment is `()`.";

pub fn parse_effect_input(input: TokenStream) -> Result<EffectKind> {
  let full = input.clone();
  let mut iter = input.into_iter().peekable();

  let mut saw_async = false;
  if peek_ident(&mut iter, "async") {
    saw_async = true;
    bump_ident(&mut iter, "async");
  }

  let mut saw_move = false;
  if peek_ident(&mut iter, "move") {
    saw_move = true;
    bump_ident(&mut iter, "move");
  }

  if saw_async && peek_group(&mut iter, Delimiter::Brace) {
    return Err(syn::Error::new(
      proc_macro2::Span::call_site(),
      ASYNC_BLOCK_UNSUPPORTED,
    ));
  }

  if peek_pipe(&mut iter) {
    let (param, env_ty) = parse_closure_param(&mut iter)?;
    let body = take_group(&mut iter, Delimiter::Brace)?;
    if iter.peek().is_some() {
      return Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "effect!: unexpected tokens after closure body",
      ));
    }

    if saw_async {
      return Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        ASYNC_CLOSURE_UNSUPPORTED,
      ));
    }
    return Ok(EffectKind::DoNotation {
      param,
      env_ty: Box::new(env_ty),
      body,
    });
  }

  if saw_async || saw_move {
    return Err(syn::Error::new(
      proc_macro2::Span::call_site(),
      "effect!: `async` is invalid here; optional `move` only before `|env: &mut R| { ... }`",
    ));
  }

  Ok(EffectKind::Bare { body: full })
}

fn peek_ident(iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>, s: &str) -> bool {
  matches!(iter.peek(), Some(TokenTree::Ident(i)) if i == s)
}

fn bump_ident(iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>, s: &str) {
  match iter.next() {
    Some(TokenTree::Ident(i)) if i == s => {}
    _ => unreachable!("bump_ident: expected {}", s),
  }
}

fn peek_group(
  iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>,
  d: Delimiter,
) -> bool {
  matches!(iter.peek(), Some(TokenTree::Group(g)) if g.delimiter() == d)
}

fn take_group(
  iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>,
  d: Delimiter,
) -> Result<TokenStream> {
  match iter.next() {
    Some(TokenTree::Group(g)) if g.delimiter() == d => Ok(g.stream()),
    Some(other) => Err(syn::Error::new_spanned(
      other,
      "effect!: expected `{ ... }`",
    )),
    None => Err(syn::Error::new(
      proc_macro2::Span::call_site(),
      "effect!: expected `{ ... }`",
    )),
  }
}

fn peek_pipe(iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>) -> bool {
  matches!(iter.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '|')
}

fn parse_closure_param(
  iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>,
) -> Result<(Ident, Type)> {
  match iter.next() {
    Some(TokenTree::Punct(p)) if p.as_char() == '|' => {}
    Some(other) => {
      return Err(syn::Error::new_spanned(
        other,
        "effect!: expected `|` to start closure parameters",
      ));
    }
    None => {
      return Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "effect!: expected `|` to start closure parameters",
      ));
    }
  }

  let mut between = Vec::new();
  loop {
    match iter.next() {
      Some(TokenTree::Punct(p)) if p.as_char() == '|' => break,
      Some(t) => between.push(t),
      None => {
        return Err(syn::Error::new(
          proc_macro2::Span::call_site(),
          "effect!: unclosed `|` in closure parameters",
        ));
      }
    }
  }

  let param_stream = TokenStream::from_iter(between);
  let param_and_ty: ClosureParam = syn::parse2(param_stream)?;
  let env_ty = strip_mut_ref_env_type(&param_and_ty.ty)?;
  Ok((param_and_ty.name, env_ty))
}

struct ClosureParam {
  name: Ident,
  ty: Type,
}

impl Parse for ClosureParam {
  fn parse(input: ParseStream) -> Result<Self> {
    let name = Ident::parse(input)?;
    <Token![:]>::parse(input)?;
    let ty = Type::parse(input)?;
    Ok(ClosureParam { name, ty })
  }
}

/// `|r: &mut R|` — `Effect` is parameterized by `R`, not `&mut R`.
fn strip_mut_ref_env_type(ty: &Type) -> Result<Type> {
  match ty {
    Type::Reference(r) if r.mutability.is_some() => Ok(*r.elem.clone()),
    _ => Err(syn::Error::new_spanned(
      ty,
      "effect!: closure parameter must be `env: &mut R` for some environment type `R`",
    )),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use proc_macro2::TokenStream;
  use quote::quote;

  mod parse_effect_input {
    use super::*;

    mod rejects_async_block {
      use super::*;

      #[test]
      fn async_brace_is_rejected_with_message() {
        let ts: TokenStream = quote! { async { 1 + 1 } };
        let err = match parse_effect_input(ts) {
          Err(e) => e,
          Ok(_) => panic!("expected parse error"),
        };
        assert!(
          err.to_string().contains("`async { ... }` is not supported"),
          "{}",
          err
        );
      }
    }

    mod classifies_do_notation {
      use super::*;

      #[test]
      fn pipe_env_and_brace_is_do_notation() {
        let ts: TokenStream = quote! { |r: &mut u32| { r } };
        match parse_effect_input(ts).unwrap() {
          EffectKind::DoNotation { param, body, .. } => {
            assert_eq!(param.to_string(), "r");
            assert!(body.to_string().contains('r'));
          }
          other => panic!("expected DoNotation: {:?}", std::mem::discriminant(&other)),
        }
      }

      #[test]
      fn move_pipe_env_is_do_notation() {
        let ts: TokenStream = quote! { move |x: &mut ()| { () } };
        assert!(matches!(
          parse_effect_input(ts).unwrap(),
          EffectKind::DoNotation { .. }
        ));
      }
    }

    mod classifies_bare_invocation {
      use super::*;

      #[test]
      fn tokens_without_async_or_pipe_are_bare() {
        let ts: TokenStream = quote! { a ~ x; 1 };
        assert!(matches!(
          parse_effect_input(ts).unwrap(),
          EffectKind::Bare { .. }
        ));
      }
    }

    mod rejects_async_closure {
      use super::*;

      #[test]
      fn async_pipe_env_is_rejected_with_message() {
        let ts: TokenStream = quote! { async |e: &mut u32| { *e } };
        let err = match parse_effect_input(ts) {
          Err(e) => e,
          Ok(_) => panic!("expected parse error"),
        };
        assert!(
          err.to_string().contains("async |...|` is not supported"),
          "{}",
          err
        );
      }
    }
  }
}
