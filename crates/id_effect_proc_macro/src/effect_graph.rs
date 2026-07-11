//! Effect Dependency Graph (EDG) analysis for auto-parallel `effect!` bind codegen.

use proc_macro2::{TokenStream, TokenTree};
use quote::quote;
use syn::Ident;

use crate::transform::{desugar_ident_tilde_bind, expand_tilde, is_capability_key_operand};

/// Planned expansion for one statement chunk (or a parallel pair).
pub enum StmtPlan {
  /// Two independent binds emitted via [`join_binds2`].
  ParallelPair {
    var0: Ident,
    operand0: TokenStream,
    var1: Ident,
    operand1: TokenStream,
  },
  /// Sequential chunk (non-bind, serial opt-out, dependency, or group > 2).
  Sequential(TokenStream),
}

struct BindStep {
  var: Ident,
  operand: TokenStream,
}

/// Analyze statement chunks (not the tail) and plan parallel/sequential expansion.
pub fn plan_statement_chunks(chunks: &[TokenStream]) -> Vec<StmtPlan> {
  let mut plans = Vec::new();
  let mut bound: Vec<Ident> = Vec::new();
  let mut i = 0usize;

  while i < chunks.len() {
    let chunk = chunks[i].clone();
    if chunk.is_empty() {
      i += 1;
      continue;
    }

    let desugared = desugar_ident_tilde_bind(chunk.clone());
    if chunk_has_serial_attr(&chunk) || chunk_has_serial_attr(&desugared) {
      plans.push(StmtPlan::Sequential(chunk));
      if let Some(step) = parse_bind_step(&desugared) {
        bound.push(step.var);
      }
      i += 1;
      continue;
    }

    let Some(step0) = parse_bind_step(&desugared) else {
      plans.push(StmtPlan::Sequential(chunk));
      i += 1;
      continue;
    };

    if bind_depends_on(&step0, &bound) {
      plans.push(StmtPlan::Sequential(chunk));
      bound.push(step0.var);
      i += 1;
      continue;
    }

    if i + 1 < chunks.len() {
      let next = chunks[i + 1].clone();
      if !next.is_empty() {
        let next_desugared = desugar_ident_tilde_bind(next.clone());
        if !chunk_has_serial_attr(&next)
          && !chunk_has_serial_attr(&next_desugared)
          && let Some(step1) = parse_bind_step(&next_desugared)
        {
          let mut deps = bound.clone();
          deps.push(step0.var.clone());
          if !bind_depends_on(&step1, &deps) {
            plans.push(StmtPlan::ParallelPair {
              var0: step0.var.clone(),
              operand0: step0.operand,
              var1: step1.var.clone(),
              operand1: step1.operand,
            });
            bound.push(step0.var);
            bound.push(step1.var);
            i += 2;
            continue;
          }
        }
      }
    }

    plans.push(StmtPlan::Sequential(chunk));
    bound.push(step0.var);
    i += 1;
  }

  plans
}

/// Emit `join_binds2` codegen for a parallel pair.
pub fn emit_parallel_pair(
  var0: &Ident,
  operand0: &TokenStream,
  var1: &Ident,
  operand1: &TokenStream,
  r: &Ident,
  path: &TokenStream,
) -> TokenStream {
  let expanded0 = expand_tilde(operand0.clone(), r, path);
  let expanded1 = expand_tilde(operand1.clone(), r, path);
  quote! {
    let (#var0, #var1) = #path::join_binds2(#expanded0, #expanded1, #r.clone())
      .await
      .map_err(#path::flatten_or)? ;
  }
}

fn parse_bind_step(chunk: &TokenStream) -> Option<BindStep> {
  let v: Vec<TokenTree> = chunk.clone().into_iter().collect();
  if v.len() < 4 {
    return None;
  }
  let TokenTree::Ident(let_kw) = &v[0] else {
    return None;
  };
  if let_kw != "let" {
    return None;
  }
  let TokenTree::Ident(var) = &v[1] else {
    return None;
  };
  let var = var.clone();
  let mut idx = 2usize;
  // Optional type annotation: `let x: Ty = ~ ...`
  if idx < v.len()
    && let TokenTree::Punct(p) = &v[idx]
    && p.as_char() == ':'
  {
    idx += 1;
    let mut angle_depth = 0usize;
    while idx < v.len() {
      match &v[idx] {
        TokenTree::Punct(p) if p.as_char() == '<' => angle_depth += 1,
        TokenTree::Punct(p) if p.as_char() == '>' && angle_depth > 0 => angle_depth -= 1,
        TokenTree::Punct(p) if p.as_char() == '=' && angle_depth == 0 => break,
        _ => {}
      }
      idx += 1;
    }
  }
  if idx >= v.len() {
    return None;
  }
  let TokenTree::Punct(eq) = &v[idx] else {
    return None;
  };
  if eq.as_char() != '=' {
    return None;
  }
  idx += 1;
  if idx >= v.len() {
    return None;
  }
  let TokenTree::Punct(tilde) = &v[idx] else {
    return None;
  };
  if tilde.as_char() != '~' {
    return None;
  }
  idx += 1;
  let operand = TokenStream::from_iter(v.into_iter().skip(idx));
  if is_capability_key_operand(&operand) {
    return None;
  }
  Some(BindStep { var, operand })
}

fn bind_depends_on(step: &BindStep, prior: &[Ident]) -> bool {
  prior
    .iter()
    .any(|name| tokens_reference_ident(&step.operand, name))
}

fn tokens_reference_ident(tokens: &TokenStream, name: &Ident) -> bool {
  for tt in tokens.clone() {
    match tt {
      TokenTree::Ident(id) if id == *name => return true,
      TokenTree::Group(g) if tokens_reference_ident(&g.stream(), name) => return true,
      _ => {}
    }
  }
  false
}

fn chunk_has_serial_attr(chunk: &TokenStream) -> bool {
  let mut iter = chunk.clone().into_iter().peekable();
  loop {
    match iter.peek() {
      Some(TokenTree::Punct(p)) if p.as_char() == '#' => {
        iter.next();
        let Some(TokenTree::Group(g)) = iter.next() else {
          return false;
        };
        if g.delimiter() == proc_macro2::Delimiter::Bracket {
          let normalized = g.stream().to_string().replace([' ', '\n', '\t'], "");
          if normalized == "effect(serial)" {
            return true;
          }
        }
      }
      Some(TokenTree::Group(_)) => {
        iter.next();
      }
      _ => break,
    }
  }
  false
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;

  #[test]
  fn independent_consecutive_binds_form_pair() {
    let chunks = vec![
      quote! { let a = ~ fetch_a() },
      quote! { let b = ~ fetch_b() },
    ];
    let plan = plan_statement_chunks(&chunks);
    assert_eq!(plan.len(), 1);
    assert!(matches!(plan[0], StmtPlan::ParallelPair { .. }));
  }

  #[test]
  fn dependent_bind_stays_sequential() {
    let chunks = vec![
      quote! { let a = ~ fetch_a() },
      quote! { let b = ~ combine(a) },
    ];
    let plan = plan_statement_chunks(&chunks);
    assert_eq!(plan.len(), 2);
    assert!(matches!(plan[0], StmtPlan::Sequential(_)));
    assert!(matches!(plan[1], StmtPlan::Sequential(_)));
  }

  #[test]
  fn serial_attr_opts_out() {
    let chunks = vec![
      quote! { #[effect(serial)] let a = ~ fetch_a() },
      quote! { let b = ~ fetch_b() },
    ];
    let plan = plan_statement_chunks(&chunks);
    assert_eq!(plan.len(), 2);
    assert!(matches!(plan[0], StmtPlan::Sequential(_)));
    assert!(matches!(plan[1], StmtPlan::Sequential(_)));
  }

  #[test]
  fn typed_bind_registers_dependency() {
    let chunks = vec![
      quote! { let filtered: Vec<i32> = ~ fetch() },
      quote! { let kept = ~ Ok(filtered.len()) },
    ];
    let plan = plan_statement_chunks(&chunks);
    assert_eq!(plan.len(), 2);
    assert!(matches!(plan[0], StmtPlan::Sequential(_)));
    assert!(matches!(plan[1], StmtPlan::Sequential(_)));
  }

  #[test]
  fn capability_key_bind_never_parallelizes() {
    let chunks = vec![quote! { let a = ~ fetch_a() }, quote! { let cap = ~ MyKey }];
    let plan = plan_statement_chunks(&chunks);
    assert_eq!(plan.len(), 2);
    assert!(matches!(plan[0], StmtPlan::Sequential(_)));
    assert!(matches!(plan[1], StmtPlan::Sequential(_)));
  }

  #[test]
  fn non_bind_chunk_breaks_parallel_group() {
    let chunks = vec![
      quote! { let a = ~ fetch_a() },
      quote! { println!("x") },
      quote! { let b = ~ fetch_b() },
    ];
    let plan = plan_statement_chunks(&chunks);
    assert_eq!(plan.len(), 3);
    assert!(matches!(plan[0], StmtPlan::Sequential(_)));
    assert!(matches!(plan[1], StmtPlan::Sequential(_)));
    assert!(matches!(plan[2], StmtPlan::Sequential(_)));
  }
}
