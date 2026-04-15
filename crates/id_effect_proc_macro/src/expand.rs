use quote::quote;

use crate::parse::EffectKind;
use crate::transform::{
  effect_body_contains_await, effect_body_contains_bind, expand_bare_body, expand_closure_body,
};

pub fn expand(kind: EffectKind) -> proc_macro2::TokenStream {
  let path = crate_path();
  match kind {
    EffectKind::DoNotation {
      param,
      env_ty,
      body,
    } => {
      let r = &param;
      let env_ty = *env_ty;
      let needs_async =
        effect_body_contains_bind(body.clone()) || effect_body_contains_await(body.clone());
      let expanded = expand_closure_body(body, r, &path);
      if needs_async {
        quote! {
          #path::Effect::new_async(move | #r: &mut #env_ty | {
            #path::box_future(async move { #expanded })
          })
        }
      } else {
        quote! {
          #path::Effect::new(move | #r: &mut #env_ty | {
            #expanded
          })
        }
      }
    }
    EffectKind::Bare { body } => {
      let needs_async =
        effect_body_contains_bind(body.clone()) || effect_body_contains_await(body.clone());
      let expanded = expand_bare_body(body, &path);
      if needs_async {
        quote! {
          #path::Effect::new_async(move |__effect_r: &mut ()| {
            #path::box_future(async move { #expanded })
          })
        }
      } else {
        quote! {
          #path::Effect::new(move |__effect_r: &mut ()| {
            #expanded
          })
        }
      }
    }
  }
}

/// We always use `::id_effect::…` so the generated code resolves in the caller's crate.
pub fn crate_path() -> proc_macro2::TokenStream {
  quote!(::id_effect)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parse::parse_effect_input;
  use quote::quote;

  fn expanded_contains_new_async(ts: &proc_macro2::TokenStream) -> bool {
    ts.to_string().contains("new_async")
  }

  fn expanded_uses_box_future(ts: &proc_macro2::TokenStream) -> bool {
    ts.to_string().contains("box_future")
  }

  #[test]
  fn bind_free_do_notation_uses_effect_new_not_async() {
    let input = quote! { |_r: &mut ()| { let x = 1; let y = 2; x + y } };
    let kind = parse_effect_input(input).expect("parse");
    let out = expand(kind);
    assert!(
      !expanded_contains_new_async(&out),
      "expected Effect::new, got: {out}"
    );
    assert!(
      !expanded_uses_box_future(&out),
      "bind-free body should not use box_future async block: {out}"
    );
  }

  #[test]
  fn bind_in_do_notation_uses_new_async() {
    let input = quote! { |_r: &mut ()| { ~fail::<(), (), ()>(()) } };
    let kind = parse_effect_input(input).expect("parse");
    let out = expand(kind);
    assert!(
      expanded_contains_new_async(&out),
      "expected new_async: {out}"
    );
    assert!(expanded_uses_box_future(&out), "expected box_future: {out}");
  }

  #[test]
  fn bind_free_bare_effect_uses_effect_new() {
    let input = quote! { 41 };
    let kind = parse_effect_input(input).expect("parse");
    let out = expand(kind);
    assert!(
      !expanded_contains_new_async(&out),
      "expected Effect::new: {out}"
    );
  }

  #[test]
  fn await_inside_async_move_closure_does_not_force_new_async() {
    use crate::transform::effect_body_contains_await;
    let body = quote! {
      let s = f(|x| async move { x.foo().await });
    };
    assert!(!effect_body_contains_await(body));
  }

  #[test]
  fn top_level_await_forces_new_async_detection() {
    use crate::transform::effect_body_contains_await;
    let body = quote! {
      foo().await
    };
    assert!(effect_body_contains_await(body));
  }
}
