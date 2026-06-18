//! Infer and validate capability keys from `effect!` bodies.

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use syn::Type;

/// Collect capability key types referenced via `require!(Key)` or `~Key` in `body`.
pub fn collect_capability_keys(body: &TokenStream) -> Vec<Type> {
  let mut keys = Vec::new();
  collect_keys_recursive(body.clone(), &mut keys);
  keys
}

fn push_unique_key(keys: &mut Vec<Type>, key: Type) {
  let name = type_key_name(&key);
  if !keys.iter().any(|k| type_key_name(k) == name) {
    keys.push(key);
  }
}

#[allow(clippy::collapsible_if)]
fn collect_keys_recursive(tokens: TokenStream, keys: &mut Vec<Type>) {
  let mut iter = tokens.into_iter().peekable();
  while let Some(tt) = iter.next() {
    match &tt {
      TokenTree::Ident(i)
        if i == "require"
          && matches!(iter.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '!') =>
      {
        if let Some(key) = parse_require_key(&mut iter) {
          push_unique_key(keys, key);
        }
      }
      TokenTree::Punct(p) if p.as_char() == '~' => {
        let operand = super::transform::collect_tilde_operand(&mut iter);
        if super::transform::is_capability_key_operand(&operand) {
          if let Ok(key) = syn::parse2::<Type>(operand) {
            push_unique_key(keys, key);
          }
        }
      }
      TokenTree::Group(g) => collect_keys_recursive(g.stream(), keys),
      _ => {}
    }
  }
}

fn parse_require_key(
  iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>,
) -> Option<Type> {
  match iter.next() {
    Some(TokenTree::Punct(p)) if p.as_char() == '!' => {}
    _ => return None,
  }
  let group = match iter.next() {
    Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis => g,
    _ => return None,
  };
  syn::parse2(group.stream()).ok()
}

/// Extract key types from `CapList<(K0, K1, …)>` or `()` / `Env`.
pub fn extract_keys_from_env_ty(env_ty: &Type) -> syn::Result<Vec<Type>> {
  match env_ty {
    Type::Tuple(t) if t.elems.is_empty() => Ok(vec![]),
    Type::Path(p) => {
      let seg = p.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(env_ty, "effect!: expected `caps!(…)` environment type")
      })?;
      if seg.ident == "Env" {
        return Ok(vec![]);
      }
      if seg.ident != "CapList" {
        return Err(syn::Error::new_spanned(
          env_ty,
          "effect!: closure environment must be `caps!(…)` or `()`",
        ));
      }
      let args = match &seg.arguments {
        syn::PathArguments::AngleBracketed(a) => a,
        _ => {
          return Err(syn::Error::new_spanned(
            env_ty,
            "effect!: expected `CapList<(…,)>`",
          ));
        }
      };
      let tuple = args
        .args
        .first()
        .and_then(|a| match a {
          syn::GenericArgument::Type(Type::Tuple(t)) => Some(t),
          _ => None,
        })
        .ok_or_else(|| syn::Error::new_spanned(env_ty, "effect!: expected `CapList<(…,)>`"))?;
      Ok(tuple.elems.iter().cloned().collect())
    }
    _ => Err(syn::Error::new_spanned(
      env_ty,
      "effect!: closure environment must be `|r: &mut caps!(…)|` or inferred `|r|`",
    )),
  }
}

fn type_key_name(ty: &Type) -> String {
  ty.to_token_stream().to_string().replace(' ', "")
}

/// When the user wrote an explicit `caps!(…)` on `r`, ensure body keys are a subset.
pub fn validate_explicit_caps(env_ty: &Type, body_keys: &[Type]) -> syn::Result<()> {
  if body_keys.is_empty() {
    return Ok(());
  }
  let explicit = extract_keys_from_env_ty(env_ty)?;
  let explicit_names: Vec<_> = explicit.iter().map(type_key_name).collect();
  for key in body_keys {
    if !explicit_names.iter().any(|n| *n == type_key_name(key)) {
      return Err(syn::Error::new_spanned(
        key,
        format!(
          "effect!: `{}` is not listed in the closure environment type",
          key.to_token_stream()
        ),
      ));
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;
  use syn::parse2;

  #[test]
  fn collect_capability_keys_from_tilde_and_require() {
    let body = quote! {
      let _a = ~AlphaKey;
      let _b = require!(BetaKey);
    };
    let keys = collect_capability_keys(&body);
    assert_eq!(keys.len(), 2);
  }

  #[test]
  fn collect_keys_ignores_non_capability_tilde() {
    let body = quote! { let x = ~42; };
    assert!(collect_capability_keys(&body).is_empty());
  }

  #[test]
  fn extract_keys_from_cap_list() {
    let ty: Type = parse2(quote! { CapList<(AlphaKey, BetaKey)> }).unwrap();
    let keys = extract_keys_from_env_ty(&ty).unwrap();
    assert_eq!(keys.len(), 2);
  }

  #[test]
  fn extract_keys_from_env_and_unit() {
    assert!(
      extract_keys_from_env_ty(&parse2(quote! { () }).unwrap())
        .unwrap()
        .is_empty()
    );
    assert!(
      extract_keys_from_env_ty(&parse2(quote! { Env }).unwrap())
        .unwrap()
        .is_empty()
    );
  }

  #[test]
  fn extract_keys_from_invalid_env_ty_errors() {
    let ty: Type = parse2(quote! { String }).unwrap();
    assert!(extract_keys_from_env_ty(&ty).is_err());
  }

  #[test]
  fn validate_explicit_caps_accepts_subset() {
    let env_ty: Type = parse2(quote! { CapList<(AlphaKey, BetaKey)> }).unwrap();
    let body_keys = vec![parse2(quote! { AlphaKey }).unwrap()];
    validate_explicit_caps(&env_ty, &body_keys).unwrap();
  }

  #[test]
  fn validate_explicit_caps_rejects_unknown_key() {
    let env_ty: Type = parse2(quote! { CapList<(AlphaKey,)> }).unwrap();
    let body_keys = vec![parse2(quote! { OtherKey }).unwrap()];
    assert!(validate_explicit_caps(&env_ty, &body_keys).is_err());
  }

  #[test]
  fn collect_capability_keys_dedupes_by_name() {
    let body = quote! {
      let _a = ~AlphaKey;
      let _b = require!(AlphaKey);
    };
    assert_eq!(collect_capability_keys(&body).len(), 1);
  }

  #[test]
  fn collect_capability_keys_in_nested_group() {
    let body = quote! { { let _ = require!(BetaKey); } };
    let keys = collect_capability_keys(&body);
    assert_eq!(keys.len(), 1);
  }

  #[test]
  fn extract_keys_from_sixteen_tuple_cap_list() {
    let ty: Type = parse2(quote! {
      CapList<(K0Key, K1Key, K2Key, K3Key, K4Key, K5Key, K6Key, K7Key, K8Key, K9Key, K10Key, K11Key, K12Key, K13Key, K14Key, K15Key)>
    }).unwrap();
    assert_eq!(extract_keys_from_env_ty(&ty).unwrap().len(), 16);
  }

  #[test]
  fn validate_explicit_caps_empty_body_ok() {
    let env_ty: Type = parse2(quote! { CapList<(AlphaKey,)> }).unwrap();
    validate_explicit_caps(&env_ty, &[]).unwrap();
  }
}
