//! Token-recursive expansion for `effect!` bodies.
//!
//! ## Operator
//!
//! `~expr` is a unary prefix "run-effect" operator — analogous to unary `-`. It expands to:
//!
//! ```text
//! (::id_effect::into_bind(expr, r).await?)
//! ```
//!
//! `~` can appear anywhere a Rust expression can appear: in `let` bindings, conditions,
//! function arguments, match arms, nested blocks, etc.
//!
//! ## Block structure
//!
//! The outermost `effect!` body is split on top-level `;` to identify statement chunks and the
//! tail expression. `~` is expanded recursively in every chunk. The tail is wrapped in
//! `Result::Ok(...)`. If the body contains no `~`, the proc macro uses `Effect::new` (sync
//! `FnOnce(&mut R) -> Result<...>`); otherwise it uses `Effect::new_async` with
//! `Box::pin(async move { ... })` so `.await` on `into_bind` is valid.
//!
//! Nested `{ }` blocks have their `~` expanded in-place but are **not** given an additional
//! `Ok(tail)` wrapper — they are plain Rust blocks, not effect blocks.
//!
//! ## Infix bind (`ident ~ expr`)
//!
//! At the start of a statement chunk, `name ~ expr` desugars to `let name = ~expr` before
//! prefix-`~` expansion (so Kelly-style `k ~ foo();` works).
//!
//! ## Precedence and method chains
//!
//! `~` binds to the immediately following primary expression (the call or path up to the
//! next call group). Method chains that follow belong to the outer expression:
//!
//! ```text
//! ~foo().bar   →   (into_bind(foo(), r).await?).bar
//! ```
//!
//! ## Known limitation
//!
//! The turbofish `>>` token (as in `Vec<Vec<u8>>`) is a single `Punct` in the token stream;
//! the angle-depth tracker does not handle it. This edge case is rare in effect call sites.

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::quote;

/// Returns `true` if the `effect!` body uses effect bind (`~`) anywhere (including nested groups).
///
/// Bind-free bodies expand to `Effect::new` instead of `Effect::new_async`, avoiding an `async`
/// state machine and `Box::pin` for the outer effect.
pub fn effect_body_contains_bind(tokens: TokenStream) -> bool {
  for tt in tokens {
    match tt {
      TokenTree::Punct(p) if p.as_char() == '~' => return true,
      TokenTree::Group(g) if effect_body_contains_bind(g.stream()) => return true,
      TokenTree::Group(_) => {}
      _ => {}
    }
  }
  false
}

/// Returns `true` if the body contains a `.await` that is **not** inside an `async` / `async move`
/// block.
///
/// `.await` nested only in `async move { ... }` (e.g. closures passed to `stream::unfold`) does
/// not require wrapping the whole `effect!` body in `async move` — the outer closure can stay
/// sync (`Effect::new`). Misclassifying those bodies as async breaks `Send` on streams that close
/// over non-`Send` effect HTTP futures.
///
/// Top-level `.await` (Drift connect, `into_bind`, etc.) must use [`Effect::new_async`].
pub fn effect_body_contains_await(tokens: TokenStream) -> bool {
  fn walk(
    mut iter: std::iter::Peekable<impl Iterator<Item = TokenTree>>,
    mut prev_dot: bool,
  ) -> bool {
    // `peek` / extra `next` in the `async` arm need the same `Peekable` as this advance.
    #[allow(clippy::while_let_on_iterator)]
    while let Some(tt) = iter.next() {
      match tt {
        TokenTree::Punct(p) if p.as_char() == '.' => {
          prev_dot = true;
        }
        TokenTree::Ident(i) if i == "await" && prev_dot => return true,
        TokenTree::Ident(i) if i == "async" => {
          prev_dot = false;
          if matches!(iter.peek(), Some(TokenTree::Ident(m)) if m == "move") {
            iter.next();
          }
          match iter.next() {
            Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Brace => {
              // Skip `async [move] { ... }` and optional `.await` on that expr (`async { }.await`).
              if matches!(iter.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '.') {
                iter.next();
                if matches!(iter.peek(), Some(TokenTree::Ident(a)) if a == "await") {
                  iter.next();
                }
              }
            }
            Some(TokenTree::Group(g)) if walk(g.stream().into_iter().peekable(), false) => {
              return true;
            }
            Some(TokenTree::Group(_)) => {}
            Some(_) => {}
            None => {}
          }
        }
        TokenTree::Ident(_) => {
          prev_dot = false;
        }
        TokenTree::Group(g) => {
          if walk(g.stream().into_iter().peekable(), false) {
            return true;
          }
          prev_dot = false;
        }
        _ => {
          prev_dot = false;
        }
      }
    }
    false
  }

  walk(tokens.into_iter().peekable(), false)
}

/// Expand an `effect!(|r: &mut R| { ... })` body.
pub fn expand_closure_body(body: TokenStream, r: &syn::Ident, path: &TokenStream) -> TokenStream {
  expand_block(body, r, path)
}

/// Expand an `effect! { ... }` (bare) body, using `__effect_r` as the environment binding.
pub fn expand_bare_body(body: TokenStream, path: &TokenStream) -> TokenStream {
  let r = syn::Ident::new("__effect_r", proc_macro2::Span::call_site());
  expand_block(body, &r, path)
}

/// When the body tail is exactly one `~operand` (after ident-desugar), the async effect body should
/// return `into_bind(operand, r).await` directly. Wrapping `Ok((into_bind(...).await?))` triggers
/// `clippy::needless_question_mark` (the `?` + outer `Ok` cancel for `Result` tails).
fn try_expand_tail_as_into_bind_await(
  tail: TokenStream,
  r: &syn::Ident,
  path: &TokenStream,
) -> Option<TokenStream> {
  let mut iter = tail.into_iter().peekable();
  match iter.next()? {
    TokenTree::Punct(p) if p.as_char() == '~' => {}
    _ => return None,
  }
  let operand = collect_tilde_operand(&mut iter);
  if iter.next().is_some() {
    return None;
  }
  let expanded_operand = expand_tilde(operand, r, path);
  Some(quote! { #path::into_bind(#expanded_operand, #r).await })
}

/// Split the outermost body on top-level `;`, expand `~` in each chunk, wrap tail in `Ok(...)`.
fn expand_block(body: TokenStream, r: &syn::Ident, path: &TokenStream) -> TokenStream {
  let chunks = split_semicolons(body);
  if chunks.is_empty() {
    return quote! { ::core::result::Result::Ok(()) };
  }

  let n = chunks.len();
  let mut stmts = Vec::new();
  for chunk in &chunks[..n - 1] {
    if chunk.is_empty() {
      continue;
    }
    let chunk = desugar_ident_tilde_bind(chunk.clone());
    let expanded = expand_tilde(chunk, r, path);
    stmts.push(quote! { #expanded ; });
  }

  let tail = &chunks[n - 1];
  if tail.is_empty() {
    quote! {
      #(#stmts)*
      ::core::result::Result::Ok(())
    }
  } else {
    let tail = desugar_ident_tilde_bind(tail.clone());
    if let Some(direct_tail) = try_expand_tail_as_into_bind_await(tail.clone(), r, path) {
      quote! {
        #(#stmts)*
        #direct_tail
      }
    } else {
      let expanded_tail = expand_tilde(tail, r, path);
      quote! {
        #(#stmts)*
        ::core::result::Result::Ok(#expanded_tail)
      }
    }
  }
}

/// `name ~ rest…` at chunk start → `let name = ~ rest…` (then [`expand_tilde`] handles `~`).
fn desugar_ident_tilde_bind(chunk: TokenStream) -> TokenStream {
  let v: Vec<TokenTree> = chunk.into_iter().collect();
  if v.len() < 2 {
    return TokenStream::from_iter(v);
  }
  match (&v[0], &v[1]) {
    (TokenTree::Ident(name), TokenTree::Punct(p)) if p.as_char() == '~' => {
      let name = name.clone();
      let rest = TokenStream::from_iter(v.into_iter().skip(2));
      quote! { let #name = ~ #rest }
    }
    _ => TokenStream::from_iter(v),
  }
}

/// Recursively walk `tokens`, rewriting every `~primary` as
/// `(::id_effect::into_bind(primary, r).await?)`.
///
/// Recurses into `{ }`, `( )`, and `[ ]` groups so `~` is expanded at any nesting depth.
/// Nested groups are **not** given `Ok(tail)` wrapping — that is only for the outermost body.
fn expand_tilde(tokens: TokenStream, r: &syn::Ident, path: &TokenStream) -> TokenStream {
  let mut out = Vec::new();
  let mut iter = tokens.into_iter().peekable();

  // Operand collection calls `next`/`peek` on the same `Peekable` as this advance.
  #[allow(clippy::while_let_on_iterator)]
  while let Some(tt) = iter.next() {
    match tt {
      TokenTree::Punct(ref p) if p.as_char() == '~' => {
        let operand = collect_tilde_operand(&mut iter);
        // Recurse so nested `~` inside the operand is also expanded.
        let expanded_operand = expand_tilde(operand, r, path);
        let bound = quote! { #path::into_bind(#expanded_operand, #r).await? };
        let group = proc_macro2::Group::new(Delimiter::Parenthesis, bound);
        out.push(TokenTree::Group(group));
      }

      TokenTree::Group(g) => {
        let expanded_inner = expand_tilde(g.stream(), r, path);
        let new_group = proc_macro2::Group::new(g.delimiter(), expanded_inner);
        out.push(TokenTree::Group(new_group));
      }

      other => out.push(other),
    }
  }

  TokenStream::from_iter(out)
}

/// Collect the operand of a `~` prefix operator.
///
/// `~` binds to the entire expression chain that follows it: idents, paths (`::`, `<...>`),
/// call/index groups, and method chains (`.method(...)`). Collection stops only at:
///
/// - `,` at depth 0 — argument separator in the surrounding call
/// - `>` at depth 0 — closing angle bracket of the enclosing turbofish
/// - end of the token stream (the containing chunk already had `;` stripped by `expand_block`)
///
/// Turbofish `::< >` sequences are tracked so commas and `>` inside generic arguments are
/// not treated as terminators.
///
/// Examples (all collected as one operand, no parens needed):
/// - `~succeed(40)` → `succeed(40)`
/// - `~raw.parse::<i32>().map_err(|e| ...)` → `raw.parse::<i32>().map_err(|e| ...)`
/// - `~parse_i32(s).map_error(f).catch(g)` → `parse_i32(s).map_error(f).catch(g)`
fn collect_tilde_operand(
  iter: &mut std::iter::Peekable<impl Iterator<Item = TokenTree>>,
) -> TokenStream {
  let mut result = Vec::new();
  let mut angle_depth: usize = 0;

  loop {
    match iter.peek() {
      None => break,

      Some(TokenTree::Punct(p)) => {
        match (p.as_char(), angle_depth) {
          // `,` at top level: argument separator — belongs to the surrounding call.
          (',', 0) => break,
          // `;` at top level: statement separator — belongs to the surrounding block.
          (';', 0) => break,
          // Open turbofish `<`.
          ('<', _) => {
            angle_depth += 1;
            result.push(iter.next().unwrap());
          }
          // Close turbofish `>` while inside angle brackets.
          ('>', d) if d > 0 => {
            angle_depth -= 1;
            result.push(iter.next().unwrap());
          }
          // `>` at depth 0 closes the surrounding turbofish — stop.
          ('>', 0) => break,
          // Everything else (`.`, `::`, `?`, operators, …) is part of the expression.
          _ => result.push(iter.next().unwrap()),
        }
      }

      // `( )` and `[ ]` groups are self-contained call-arg / index expressions — consume them.
      // `{ }` brace groups are block statements that follow an expression, not part of it — stop.
      Some(TokenTree::Group(g)) if g.delimiter() != Delimiter::Brace => {
        result.push(iter.next().unwrap());
      }
      Some(TokenTree::Group(_)) => break,

      Some(_) => result.push(iter.next().unwrap()),
    }
  }

  TokenStream::from_iter(result)
}

/// Returns `true` when `tt` is an identifier that starts a Rust block expression:
/// `if`, `while`, `for`, `loop`, `match`, `async`, or `unsafe`.
/// These expressions end with a `}` brace group and can be used as statements
/// without a trailing `;`, unlike ordinary expressions.
fn is_block_stmt_keyword(tt: &TokenTree) -> bool {
  match tt {
    TokenTree::Ident(id) => matches!(
      id.to_string().as_str(),
      "if" | "while" | "for" | "loop" | "match" | "async" | "unsafe"
    ),
    _ => false,
  }
}

/// Split `stream` on top-level `;`. Groups are opaque — inner `;` are not split on.
///
/// Additionally, a top-level `{ }` brace group that closes a **block-type expression**
/// (one whose first token is `if`, `while`, `for`, `loop`, `match`, `async`, or `unsafe`)
/// is treated as an implicit statement boundary, matching Rust's rule that these
/// expressions can appear as statements without a trailing `;`.
///
/// The split is suppressed when:
/// - the next token is `else` (the expression continues with an else-branch), or
/// - the brace group is the last token in the stream (it is the tail expression, not a statement).
fn split_semicolons(stream: TokenStream) -> Vec<TokenStream> {
  let mut out = Vec::new();
  let mut cur: Vec<TokenTree> = Vec::new();
  let mut iter = stream.into_iter().peekable();

  // Brace handling uses `peek` on the token after the current one.
  #[allow(clippy::while_let_on_iterator)]
  while let Some(tt) = iter.next() {
    let is_semicolon = matches!(&tt, TokenTree::Punct(p) if p.as_char() == ';');
    let is_brace = matches!(&tt, TokenTree::Group(g) if g.delimiter() == Delimiter::Brace);

    if is_semicolon {
      out.push(TokenStream::from_iter(cur.drain(..)));
    } else if is_brace {
      let first_is_block_kw = cur.first().is_some_and(is_block_stmt_keyword);
      cur.push(tt);
      let next = iter.peek();
      let continues_with_else =
        matches!(next, Some(TokenTree::Ident(id)) if id.to_string().as_str() == "else");
      let at_end_of_stream = next.is_none();
      if first_is_block_kw && !continues_with_else && !at_end_of_stream {
        out.push(TokenStream::from_iter(cur.drain(..)));
      }
    } else {
      cur.push(tt);
    }
  }

  out.push(TokenStream::from_iter(cur));
  out
}

#[cfg(test)]
mod tests {
  #![allow(unused_imports)]

  //! Coverage map for [`super`] (token transform for `effect!` bodies).
  //!
  //! ## Entry points
  //! - [`expand_bare_body`]: fixed env ident `__effect_r` (bare `effect! { ... }`).
  //! - [`expand_closure_body`]: caller-supplied `r` (`effect!(|r: &mut R| { ... })`).
  //!
  //! Both delegate to [`expand_block`].
  //!
  //! ## [`expand_block`] (combine with `split_semicolons` + per-chunk pipeline)
  //! - **`chunks.is_empty()`** — defensive; [`split_semicolons`] always pushes at least one chunk
  //!   (even for an empty stream), so this path is not reachable from the public expanders today.
  //! - **`n == 1`, tail empty** — e.g. empty body, or only top-level `;` / `;;` leaving final chunk empty.
  //! - **`n == 1`, tail non-empty** — single chunk, tail wrapped in `Result::Ok(tail)`.
  //! - **`n > 1`** — statement chunks: each non-empty chunk → `desugar_ident_tilde_bind` → `expand_tilde` →
  //!   stmt + `;`; last chunk is tail (`Ok` or `Ok(())` if empty).
  //! - **Empty non-final chunks** — skipped (`continue`), e.g. `;;` or leading `;`.
  //!
  //! ## [`split_semicolons`] (top-level `;` only)
  //! - Split on `TokenTree::Punct` `;` **outside** any `Group`; delimiters `{ }` `( )` `[ ]` are opaque.
  //!
  //! ## [`desugar_ident_tilde_bind`] (infix bind at chunk start)
  //! - **`< 2` tokens** — passthrough unchanged.
  //! - **`ident` + `~`** — rewrite to `let ident = ~ rest`.
  //! - **Else** — passthrough (not `ident~` at start).
  //!
  //! ## [`expand_tilde`] (prefix `~` anywhere in token tree)
  //! - **`~`** — replace with `(path::into_bind(operand, r).await?)` (parenthesized group); operand from
  //!   [`collect_tilde_operand`]; recurse into operand. The **sole** body tail `~operand` (see
  //!   [`try_expand_tail_as_into_bind_await`]) omits the inner `?` and outer `Ok` so the async block
  //!   returns `Result` directly (avoids `clippy::needless_question_mark`).
  //! - **`Group`** — recurse into inner stream, same delimiter.
  //! - **Other tokens** — copy through.
  //!
  //! ## [`collect_tilde_operand`] (what binds to prefix `~`)
  //! - **End of stream** — operand ends.
  //! - **`,` at `angle_depth == 0`** — stop (outer arg separator).
  //! - **`<`** — increment `angle_depth` (turbofish / generics).
  //! - **`>` with `angle_depth > 0`** — decrement (close generic angle).
  //! - **`>` with `angle_depth == 0`** — stop (closing enclosing turbofish from caller context).
  //! - **Any other punct** — part of operand (e.g. `.`, `::`, `?`).
  //! - **`Group`** — always consume whole group.
  //! - **Non-punct** — consume (ident, literal, …).
  //! - **Known limitation** — `>>` in `Vec<Vec<u8>>` is one `Punct` token; depth tracker does not model it.

  use super::*;
  use quote::quote;

  macro_rules! assert_ts_eq {
    ($a:expr, $e:expr) => {{
      let a = ($a).to_string();
      let e = ($e).to_string();
      assert_eq!(a, e, "\nactual:\n{a}\n\nexpected:\n{e}\n");
    }};
  }

  /// [`collect_tilde_operand`] expects the iterator *after* the `~` token.
  fn operand_after_tilde_prefix(full: TokenStream) -> TokenStream {
    let mut iter = full.into_iter().peekable();
    match iter.next() {
      Some(TokenTree::Punct(p)) if p.as_char() == '~' => collect_tilde_operand(&mut iter),
      o => panic!("expected leading `~`, got {o:?}"),
    }
  }

  fn r_custom() -> syn::Ident {
    syn::Ident::new("my_r", proc_macro2::Span::call_site())
  }

  #[test]
  fn effect_body_contains_bind_false_without_tilde() {
    assert!(!effect_body_contains_bind(quote! { let x = 1; x + 2 }));
  }

  #[test]
  fn effect_body_contains_bind_true_with_prefix_tilde() {
    assert!(effect_body_contains_bind(quote! { ~g() }));
  }

  #[test]
  fn effect_body_contains_bind_true_in_nested_group() {
    assert!(effect_body_contains_bind(
      quote! { if true { ~x() } else { 0 } }
    ));
  }

  // ---------------------------------------------------------------------------
  // expand_bare_body — integration: `__effect_r` + expand_block
  // ---------------------------------------------------------------------------
  mod expand_bare_body {
    use super::*;

    /// Branch: single final chunk empty after split → `Result::Ok(())`, no statements.
    mod with_empty_body {
      use super::*;

      #[test]
      fn returns_ok_unit() {
        let path = quote! { ::id_effect };
        let body = TokenStream::new();
        let out = expand_bare_body(body, &path);
        let expected = quote! { ::core::result::Result::Ok(()) };
        assert_eq!(out.to_string(), expected.to_string());
      }
    }

    /// Branch: `n == 1`, tail non-empty, no `~` → `Ok(tail)` only.
    mod with_tail_having_no_tilde_operator {
      use super::*;

      #[test]
      fn wraps_expression_in_ok() {
        let path = quote! { ::id_effect };
        let body = quote! { 41 };
        let out = expand_bare_body(body, &path);
        let expected = quote! { ::core::result::Result::Ok(41) };
        assert_eq!(out.to_string(), expected.to_string());
      }
    }

    /// Permutation: one top-level `;` with nothing after → final chunk empty → `Ok(())`.
    mod with_trailing_top_level_semicolon_only {
      use super::*;

      #[test]
      fn yields_ok_unit_with_empty_tail() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ; }, &path);
        let expected = quote! { ::core::result::Result::Ok(()) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Permutation: `stmt;` then tail — one statement chunk + final tail chunk.
    mod with_one_statement_chunk_then_tail {
      use super::*;

      #[test]
      fn emits_statement_then_ok_wrapped_tail() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { let _x = 1 ; 2 }, &path);
        let expected = quote! {
          let _x = 1 ;
          ::core::result::Result::Ok(2)
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Permutation: multiple `stmt;` … `tail` — several non-final chunks + tail.
    mod with_multiple_statement_chunks_then_tail {
      use super::*;

      #[test]
      fn emits_each_statement_then_ok_wrapped_tail() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { 1 ; 2 ; 3 }, &path);
        let expected = quote! {
          1 ;
          2 ;
          ::core::result::Result::Ok(3)
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Branch: empty non-final chunks skipped — e.g. leading `;`, `;;`, or `a;;b` middle empty chunk.
    mod with_empty_chunks_from_leading_or_duplicate_semicolons {
      use super::*;

      #[test]
      fn skips_empty_first_chunk() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ; 1 }, &path);
        let expected = quote! { ::core::result::Result::Ok(1) };
        assert_ts_eq!(out, expected);
      }

      #[test]
      fn skips_empty_middle_chunks() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { 1 ;; 2 }, &path);
        let expected = quote! {
          1 ;
          ::core::result::Result::Ok(2)
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Permutation: `;` only inside `{ ... }` — no extra top-level chunks from inner `;`.
    mod with_semicolon_only_inside_brace_group {
      use super::*;

      #[test]
      fn does_not_split_on_inner_semicolon() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { { 1 ; 2 } }, &path);
        let expected = quote! { ::core::result::Result::Ok({ 1 ; 2 }) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Permutation: inner `;` inside `( ... )` / `[ ... ]` — same opacity as braces.
    mod with_semicolon_only_inside_paren_or_bracket_group {
      use super::*;

      #[test]
      fn does_not_split_inside_paren_group() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ( 1 ; 2 ) }, &path);
        let expected = quote! { ::core::result::Result::Ok(( 1 ; 2 )) };
        assert_ts_eq!(out, expected);
      }

      #[test]
      fn does_not_split_inside_bracket_group() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { [ 1 ; 2 ] }, &path);
        let expected = quote! { ::core::result::Result::Ok([ 1 ; 2 ]) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: `ident ~ expr` at start of a **statement** chunk → `let` + prefix `~` expansion.
    mod with_ident_tilde_bind_on_statement_chunk {
      use super::*;

      #[test]
      fn desugars_to_let_and_into_bind() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { k ~ foo ( ) ; 1 }, &path);
        let expected = quote! {
          let k = (::id_effect::into_bind(foo(), __effect_r).await?) ;
          ::core::result::Result::Ok(1)
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: `ident ~ expr` when that chunk is the **tail** — same desugar, tail wrapped in `Ok`.
    mod with_ident_tilde_bind_on_tail_chunk {
      use super::*;

      #[test]
      fn desugars_and_wraps_tail_in_ok() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { k ~ foo ( ) }, &path);
        let expected = quote! {
          ::core::result::Result::Ok(let k = (::id_effect::into_bind(foo(), __effect_r).await?))
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: prefix `~` only on tail — `into_bind(..., __effect_r)`.
    mod with_prefix_tilde_on_tail_simple_call {
      use super::*;

      #[test]
      fn expands_tilde_tail_to_into_bind_await_without_ok_wrap() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ~ foo ( ) }, &path);
        let expected = quote! {
          ::id_effect::into_bind(foo(), __effect_r).await
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: `~path::segment(args)` / `~ident` — operand collection to end or group boundary.
    mod with_prefix_tilde_path_and_call_shapes {
      use super::*;

      #[test]
      fn expands_path_call() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ~ bar :: baz ( ) }, &path);
        let expected = quote! {
          ::id_effect::into_bind(bar::baz(), __effect_r).await
        };
        assert_ts_eq!(out, expected);
      }

      #[test]
      fn expands_plain_ident_operand() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ~ x }, &path);
        let expected = quote! {
          ::id_effect::into_bind(x, __effect_r).await
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: turbofish `~expr::method::<T>()` — angle depth in [`collect_tilde_operand`].
    mod with_prefix_tilde_turbofish_and_method_chain {
      use super::*;

      #[test]
      fn includes_turbofish_and_following_method_chain_in_operand() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(
          quote! { ~ raw . parse :: < i32 > ( ) . map_err ( | e | e ) },
          &path,
        );
        let expected = quote! {
          ::id_effect::into_bind(raw.parse::<i32>().map_err(|e| e), __effect_r).await
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Operand stops at outer comma inside a parenthesized group (depth-0 comma).
    mod with_prefix_tilde_comma_terminator_at_depth_zero {
      use super::*;

      #[test]
      fn truncates_operand_before_following_comma() {
        let path = quote! { ::id_effect };
        let r = syn::Ident::new("__effect_r", proc_macro2::Span::call_site());
        let out = expand_tilde(quote! { ( ~ x , y ) }, &r, &path);
        let expected = quote! { ((::id_effect::into_bind(x, __effect_r).await?), y) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: nested `~` inside operand (recursion in [`expand_tilde`]).
    mod with_nested_prefix_tilde_inside_operand {
      use super::*;

      #[test]
      fn expands_inner_tilde_first() {
        let path = quote! { ::id_effect };
        let r = syn::Ident::new("__effect_r", proc_macro2::Span::call_site());
        let out = expand_tilde(quote! { ~ ~ foo ( ) }, &r, &path);
        let expected = quote! {
          (::id_effect::into_bind((::id_effect::into_bind(foo(), __effect_r).await?), __effect_r).await?)
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Syntax: `~` inside nested `{ }` / `( )` / `[ ]` — expanded in place, **no** extra outer `Ok` on block.
    mod with_prefix_tilde_inside_nested_groups {
      use super::*;

      #[test]
      fn expands_inside_brace_group() {
        let path = quote! { ::id_effect };
        let r = syn::Ident::new("__effect_r", proc_macro2::Span::call_site());
        let out = expand_tilde(quote! { { ~ a ( ) } }, &r, &path);
        let expected = quote! { { (::id_effect::into_bind(a(), __effect_r).await?) } };
        assert_ts_eq!(out, expected);
      }

      #[test]
      fn expands_inside_paren_group() {
        let path = quote! { ::id_effect };
        let r = syn::Ident::new("__effect_r", proc_macro2::Span::call_site());
        let out = expand_tilde(quote! { ( ~ b ( ) ) }, &r, &path);
        let expected = quote! { ((::id_effect::into_bind(b(), __effect_r).await?)) };
        assert_ts_eq!(out, expected);
      }

      #[test]
      fn expands_inside_bracket_group() {
        let path = quote! { ::id_effect };
        let r = syn::Ident::new("__effect_r", proc_macro2::Span::call_site());
        let out = expand_tilde(quote! { [ ~ c ( ) ] }, &r, &path);
        let expected = quote! { [(::id_effect::into_bind(c(), __effect_r).await?)] };
        assert_ts_eq!(out, expected);
      }
    }

    /// Permutation: non-`::effect` `path` token tree — `into_bind` uses provided path prefix.
    mod with_custom_crate_path_token_stream {
      use super::*;

      #[test]
      fn uses_path_prefix_in_into_bind() {
        let path = quote! { ::my_effect };
        let out = expand_bare_body(quote! { ~ z ( ) }, &path);
        let expected = quote! {
          ::my_effect::into_bind(z(), __effect_r).await
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Two separate `>` tokens: collection stops after the first closing `>`, leaving `> ()` outside
    /// the `into_bind` operand (see module docs on turbofish / `>>`).
    mod with_double_angle_bracket_turbofish_limitation {
      use super::*;

      #[test]
      fn does_not_match_ideal_operand_boundary() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { ~ v :: < u8 > > ( ) }, &path);
        let expected = quote! {
          ::core::result::Result::Ok((::id_effect::into_bind(v::<u8>, __effect_r).await?) > ())
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// `if cond { ~f(); } tail` — the idiomatic Rust pattern: an `if` block used as
    /// a statement (no trailing `;` needed) followed by a return-value tail expression.
    /// `~` inside the if-body are expanded; the tail is wrapped in `Ok(...)`.
    mod with_if_block_statement_then_tail {
      use super::*;

      #[test]
      fn expands_tilde_in_if_body_and_ok_wraps_tail() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(quote! { if cond { ~ f ( ) ; } A :: default ( ) }, &path);
        let expected = quote! {
          if cond { (::id_effect::into_bind(f(), __effect_r).await?) ; } ;
          ::core::result::Result::Ok(A :: default ())
        };
        assert_ts_eq!(out, expected);
      }

      /// Multiple `~` statements inside the if-body, all expanded correctly.
      #[test]
      fn expands_multiple_tildes_in_if_body_and_ok_wraps_tail() {
        let path = quote! { ::id_effect };
        let out = expand_bare_body(
          quote! { if n < 2 { ~ log_warn ( "msg" ) ; ~ fail ( err ) ; } A :: default ( ) },
          &path,
        );
        let expected = quote! {
          if n < 2 {
            (::id_effect::into_bind(log_warn("msg"), __effect_r).await?) ;
            (::id_effect::into_bind(fail(err), __effect_r).await?) ;
          } ;
          ::core::result::Result::Ok(A :: default ())
        };
        assert_ts_eq!(out, expected);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // expand_closure_body — integration: caller-supplied `r` ident (same block logic as bare)
  // ---------------------------------------------------------------------------
  mod expand_closure_body {
    use super::*;

    /// Same as bare empty body but `into_bind(..., r)` must use the given ident, not `__effect_r`.
    mod with_empty_body {
      use super::*;

      #[test]
      fn yields_ok_unit_using_param_ident() {
        let path = quote! { ::id_effect };
        let r = r_custom();
        let out = expand_closure_body(TokenStream::new(), &r, &path);
        let expected = quote! { ::core::result::Result::Ok(()) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Same as bare literal tail; closure `r` appears in `into_bind` when `~` is present.
    mod with_tail_having_no_tilde_operator {
      use super::*;

      #[test]
      fn wraps_expression_in_ok_like_bare() {
        let path = quote! { ::id_effect };
        let r = r_custom();
        let out = expand_closure_body(quote! { 41 }, &r, &path);
        let expected = quote! { ::core::result::Result::Ok(41) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Parity: statement + tail with custom `r`.
    mod with_one_statement_chunk_then_tail {
      use super::*;

      #[test]
      fn emits_statement_then_ok_wrapped_tail() {
        let path = quote! { ::id_effect };
        let r = r_custom();
        let out = expand_closure_body(quote! { 1 ; 2 }, &r, &path);
        let expected = quote! {
          1 ;
          ::core::result::Result::Ok(2)
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Parity: `~` expansion uses custom `r`.
    mod with_prefix_tilde_on_tail {
      use super::*;

      #[test]
      fn into_bind_second_arg_matches_param_ident() {
        let path = quote! { ::id_effect };
        let r = r_custom();
        let out = expand_closure_body(quote! { ~ g ( ) }, &r, &path);
        let expected = quote! {
          ::id_effect::into_bind(g(), my_r).await
        };
        assert_ts_eq!(out, expected);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // expand_block — direct (private); mirrors branches when public expanders are insufficient
  // (Module name avoids shadowing [`super::expand_block`].)
  // ---------------------------------------------------------------------------
  mod expand_block_branches {
    use super::*;

    /// Invariant: [`split_semicolons`] always yields ≥ 1 chunk, so `expand_block`'s `chunks.is_empty()`
    /// arm is not reachable from the public expanders today.
    mod defensive_empty_chunks {
      use super::*;

      #[test]
      fn split_semicolons_never_returns_empty_vec() {
        assert!(!split_semicolons(TokenStream::new()).is_empty());
      }
    }

    /// Direct check: custom `r` + path with non-empty multi-chunk body.
    mod with_explicit_r_and_path {
      use super::*;

      #[test]
      fn expands_like_integration() {
        let path = quote! { ::id_effect };
        let r = r_custom();
        let out = expand_block(quote! { 1 ; 2 }, &r, &path);
        let expected = quote! {
          1 ;
          ::core::result::Result::Ok(2)
        };
        assert_ts_eq!(out, expected);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // split_semicolons — direct (module name avoids shadowing [`super::split_semicolons`].)
  // ---------------------------------------------------------------------------
  mod semicolon_splitting {
    use super::*;

    /// Branch: empty stream → exactly one empty chunk (so `expand_block` sees tail empty).
    mod empty_stream {
      use super::*;

      #[test]
      fn yields_single_empty_chunk() {
        let chunks = split_semicolons(TokenStream::new());
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].is_empty());
      }
    }

    /// Branch: no `;` → single chunk equals input.
    mod no_semicolon {
      use super::*;

      #[test]
      fn yields_single_chunk_identical_to_input() {
        let input = quote! { a + b };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }
    }

    /// Branch: one top-level `;` → two chunks.
    mod one_top_level_semicolon {
      use super::*;

      #[test]
      fn yields_two_chunks() {
        let chunks = split_semicolons(quote! { a ; b });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { a });
        assert_ts_eq!(chunks[1].clone(), quote! { b });
      }
    }

    /// Branch: multiple top-level `;` → `k + 1` chunks for `k` separators.
    mod multiple_top_level_semicolons {
      use super::*;

      #[test]
      fn yields_one_more_chunk_than_separator_count() {
        let chunks = split_semicolons(quote! { a ; b ; c });
        assert_eq!(chunks.len(), 3);
        assert_ts_eq!(chunks[0].clone(), quote! { a });
        assert_ts_eq!(chunks[1].clone(), quote! { b });
        assert_ts_eq!(chunks[2].clone(), quote! { c });
      }
    }

    /// Permutation: trailing `;` → final chunk empty.
    mod trailing_semicolon {
      use super::*;

      #[test]
      fn last_chunk_empty() {
        let chunks = split_semicolons(quote! { x ; });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { x });
        assert!(chunks[1].is_empty());
      }
    }

    /// Permutation: leading `;` → first chunk empty.
    mod leading_semicolon {
      use super::*;

      #[test]
      fn first_chunk_empty() {
        let chunks = split_semicolons(quote! { ; y });
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].is_empty());
        assert_ts_eq!(chunks[1].clone(), quote! { y });
      }
    }

    /// Permutation: `;` nested in `{ }` — not a separator.
    mod semicolon_inside_brace_group {
      use super::*;

      #[test]
      fn single_top_level_chunk() {
        let input = quote! { { a ; b } };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }
    }

    /// Permutation: `;` nested in `( )`.
    mod semicolon_inside_paren_group {
      use super::*;

      #[test]
      fn single_top_level_chunk() {
        let input = quote! { ( a ; b ) };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }
    }

    /// Permutation: `;` nested in `[ ]`.
    mod semicolon_inside_bracket_group {
      use super::*;

      #[test]
      fn single_top_level_chunk() {
        let input = quote! { [ a ; b ] };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }
    }

    /// Block-type expressions (`if`, `while`, …) used as statements without `;`.
    /// In Rust these can omit the trailing semicolon; the macro must recognise the
    /// implicit statement boundary so the following expression becomes the tail.
    mod block_expression_implicit_statement_boundary {
      use super::*;

      /// `if cond { body } tail` — the brace closes the if-expression; `tail` is a
      /// new chunk (the tail expression to be wrapped in `Ok`).
      #[test]
      fn if_block_followed_by_expression_splits_into_two_chunks() {
        let chunks = split_semicolons(quote! { if cond { body } tail });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { if cond { body } });
        assert_ts_eq!(chunks[1].clone(), quote! { tail });
      }

      /// `if cond { x } else { y } tail` — `else { y }` continues the expression,
      /// so there is no split after `{ x }`.  Split happens after `{ y }`.
      #[test]
      fn if_else_block_followed_by_expression_splits_after_else_branch() {
        let chunks = split_semicolons(quote! { if cond { x } else { y } tail });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { if cond { x } else { y } });
        assert_ts_eq!(chunks[1].clone(), quote! { tail });
      }

      /// `if cond { x } else if d { y } tail` — chained else-if, single split after the last `}`.
      #[test]
      fn chained_else_if_block_splits_after_final_branch() {
        let chunks = split_semicolons(quote! { if a { x } else if b { y } tail });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { if a { x } else if b { y } });
        assert_ts_eq!(chunks[1].clone(), quote! { tail });
      }

      /// `if cond { body }` as the **sole** expression — it is the tail, NOT a statement.
      /// No split should occur; the single chunk is wrapped in `Ok(...)` by `expand_block`.
      #[test]
      fn if_block_as_sole_expression_stays_as_single_chunk() {
        let input = quote! { if cond { body } };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }

      /// `if cond { x } else { y }` as the sole expression — no following tokens, so
      /// no split; the whole if-else is the tail.
      #[test]
      fn if_else_block_as_sole_expression_stays_as_single_chunk() {
        let input = quote! { if cond { x } else { y } };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }

      /// `while cond { body } tail` — same implicit-boundary rule for `while`.
      #[test]
      fn while_block_followed_by_expression_splits() {
        let chunks = split_semicolons(quote! { while cond { body } tail });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { while cond { body } });
        assert_ts_eq!(chunks[1].clone(), quote! { tail });
      }

      /// `match expr { arms } tail` — same for `match`.
      #[test]
      fn match_block_followed_by_expression_splits() {
        let chunks = split_semicolons(quote! { match expr { arms } tail });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { match expr { arms } });
        assert_ts_eq!(chunks[1].clone(), quote! { tail });
      }

      /// A standalone `{ block }` group (no preceding keyword) is never auto-split —
      /// the user must add `;` explicitly to make it a statement.
      #[test]
      fn bare_brace_block_without_keyword_is_not_auto_split() {
        let input = quote! { { a ; b } };
        let chunks = split_semicolons(input.clone());
        assert_eq!(chunks.len(), 1);
        assert_ts_eq!(chunks[0].clone(), input);
      }

      /// `let x = { foo() }; tail` — the brace is inside a `let`, which starts the
      /// chunk; `let` is not a block keyword so no auto-split at the brace.  The `;`
      /// is the real separator.
      #[test]
      fn let_with_brace_rhs_splits_at_semicolon_not_at_brace() {
        let chunks = split_semicolons(quote! { let x = { foo ( ) } ; tail });
        assert_eq!(chunks.len(), 2);
        assert_ts_eq!(chunks[0].clone(), quote! { let x = { foo ( ) } });
        assert_ts_eq!(chunks[1].clone(), quote! { tail });
      }
    }
  }

  // ---------------------------------------------------------------------------
  // desugar_ident_tilde_bind — direct (module name avoids shadowing [`super::desugar_ident_tilde_bind`].)
  // ---------------------------------------------------------------------------
  mod ident_tilde_desugar {
    use super::*;

    /// Branch: empty stream → unchanged.
    mod empty_chunk {
      use super::*;

      #[test]
      fn passthrough() {
        let input = TokenStream::new();
        assert_ts_eq!(desugar_ident_tilde_bind(input.clone()), input);
      }
    }

    /// Branch: single token → unchanged (`len < 2`).
    mod single_token {
      use super::*;

      #[test]
      fn passthrough() {
        let input = quote! { x };
        assert_ts_eq!(desugar_ident_tilde_bind(input.clone()), input);
      }
    }

    /// Branch: `ident` + punct other than `~` → unchanged.
    mod ident_then_non_tilde_punct {
      use super::*;

      #[test]
      fn passthrough() {
        let input = quote! { a + b };
        assert_ts_eq!(desugar_ident_tilde_bind(input.clone()), input);
      }
    }

    /// Branch: non-ident first token (literal, group, punct) + `~` → unchanged.
    mod non_ident_first_token {
      use super::*;

      #[test]
      fn passthrough() {
        let input = quote! { 1 ~ x };
        assert_ts_eq!(desugar_ident_tilde_bind(input.clone()), input);
      }
    }

    /// Branch: `ident ~ rest` → `let ident = ~ rest`.
    mod ident_tilde_at_start {
      use super::*;

      #[test]
      fn rewrites_to_let_with_prefix_tilde() {
        let out = desugar_ident_tilde_bind(quote! { k ~ foo ( ) });
        let expected = quote! { let k = ~ foo ( ) };
        assert_ts_eq!(out, expected);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // expand_tilde — direct (module name avoids shadowing [`super::expand_tilde`].)
  // ---------------------------------------------------------------------------
  mod tilde_expansion {
    use super::*;

    fn r_env() -> syn::Ident {
      syn::Ident::new("r_env", proc_macro2::Span::call_site())
    }

    fn path_effect() -> TokenStream {
      quote! { ::id_effect }
    }

    /// Branch: empty → empty.
    mod empty_stream {
      use super::*;

      #[test]
      fn unchanged() {
        let r = r_env();
        let path = path_effect();
        let input = TokenStream::new();
        assert_ts_eq!(expand_tilde(input.clone(), &r, &path), input);
      }
    }

    /// Branch: no `~` → unchanged (modulo group recursion copying structure).
    mod no_tilde_operator {
      use super::*;

      #[test]
      fn passthrough() {
        let r = r_env();
        let path = path_effect();
        let input = quote! { a + b };
        assert_ts_eq!(expand_tilde(input.clone(), &r, &path), input);
      }
    }

    /// Branch: one `~` + operand.
    mod single_prefix_tilde {
      use super::*;

      #[test]
      fn wraps_into_bind_in_parens() {
        let r = r_env();
        let path = path_effect();
        let out = expand_tilde(quote! { ~ foo ( ) }, &r, &path);
        let expected = quote! { (::id_effect::into_bind(foo(), r_env).await?) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Branch: two `~` in sequence (two operands).
    mod two_prefix_tildes {
      use super::*;

      #[test]
      fn expands_both_operands() {
        let r = r_env();
        let path = path_effect();
        // `;` is not an operand boundary for `collect_tilde_operand`; `,` at depth 0 is.
        let out = expand_tilde(quote! { ~ f ( ) , ~ g ( ) }, &r, &path);
        let expected = quote! {
          (::id_effect::into_bind(f(), r_env).await?) , (::id_effect::into_bind(g(), r_env).await?)
        };
        assert_ts_eq!(out, expected);
      }

      /// Two `~` separated by `;` inside a brace group — each should expand independently.
      /// Both `~f()` and `~g()` must become separate `into_bind` calls; the `;` is a
      /// statement separator, not part of either operand.
      ///
      /// Currently FAILS: `;` is not a stop character in `collect_tilde_operand`, so the
      /// first `~` swallows `f() ; ~g()` as one operand.
      #[test]
      fn expands_both_operands_separated_by_semicolon_inside_block() {
        let r = r_env();
        let path = path_effect();
        let out = expand_tilde(quote! { { ~ f ( ) ; ~ g ( ) } }, &r, &path);
        let expected = quote! {
          { (::id_effect::into_bind(f(), r_env).await?) ; (::id_effect::into_bind(g(), r_env).await?) }
        };
        assert_ts_eq!(out, expected);
      }
    }

    /// Branch: `~` inside `{ ... }` — recurse, no Ok-wrap here.
    mod tilde_inside_brace_group {
      use super::*;

      #[test]
      fn expands_inside_group() {
        let r = r_env();
        let path = path_effect();
        let out = expand_tilde(quote! { { ~ a ( ) } }, &r, &path);
        let expected = quote! { { (::id_effect::into_bind(a(), r_env).await?) } };
        assert_ts_eq!(out, expected);
      }
    }

    /// Branch: `~` inside `( ... )`.
    mod tilde_inside_paren_group {
      use super::*;

      #[test]
      fn expands_inside_group() {
        let r = r_env();
        let path = path_effect();
        let out = expand_tilde(quote! { ( ~ b ( ) ) }, &r, &path);
        let expected = quote! { ((::id_effect::into_bind(b(), r_env).await?)) };
        assert_ts_eq!(out, expected);
      }
    }

    /// Branch: `~` inside `[ ... ]`.
    mod tilde_inside_bracket_group {
      use super::*;

      #[test]
      fn expands_inside_group() {
        let r = r_env();
        let path = path_effect();
        let out = expand_tilde(quote! { [ ~ c ( ) ] }, &r, &path);
        let expected = quote! { [(::id_effect::into_bind(c(), r_env).await?)] };
        assert_ts_eq!(out, expected);
      }
    }

    /// Branch: nested groups with multiple `~`.
    mod nested_groups_with_multiple_tildes {
      use super::*;

      #[test]
      fn expands_depth_first_operands() {
        let r = r_env();
        let path = path_effect();
        let out = expand_tilde(quote! { { ~ u ( ) , ~ v ( ) } }, &r, &path);
        let expected = quote! {
          { (::id_effect::into_bind(u(), r_env).await?) , (::id_effect::into_bind(v(), r_env).await?) }
        };
        assert_ts_eq!(out, expected);
      }
    }
  }

  // ---------------------------------------------------------------------------
  // collect_tilde_operand — direct (module name avoids shadowing [`super::collect_tilde_operand`].)
  // ---------------------------------------------------------------------------
  mod tilde_operand_collection {
    use super::*;

    /// Operand empty (immediate `,` / `>` / end) — edge case.
    mod empty_operand {
      use super::*;

      #[test]
      fn collects_nothing_before_terminator_end() {
        assert!(operand_after_tilde_prefix(quote! { ~ }).is_empty());
      }

      #[test]
      fn collects_nothing_before_comma() {
        assert!(operand_after_tilde_prefix(quote! { ~ , y }).is_empty());
      }

      #[test]
      fn collects_nothing_before_gt_at_depth_zero() {
        assert!(operand_after_tilde_prefix(quote! { ~ > z }).is_empty());
      }
    }

    /// Stops at end of chunk stream.
    mod ends_at_eof {
      use super::*;

      #[test]
      fn collects_full_remaining_tokens() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ foo ( ) }),
          quote! { foo ( ) }
        );
      }
    }

    /// `,` at angle depth 0 stops.
    mod comma_at_depth_zero {
      use super::*;

      #[test]
      fn does_not_include_following_comma_or_args() {
        assert_ts_eq!(operand_after_tilde_prefix(quote! { ~ a , b }), quote! { a });
      }
    }

    /// `<` / `>` pairing inside generics.
    mod turbofish_angle_depth {
      use super::*;

      #[test]
      fn keeps_commas_inside_angles_in_operand() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ p :: < T , U > ( ) }),
          quote! { p :: < T , U > ( ) }
        );
      }

      #[test]
      fn closes_each_angle_with_greater_than() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ q :: < Vec < u8 > > ( ) }),
          quote! { q :: < Vec < u8 > > ( ) }
        );
      }
    }

    /// `>` at depth 0 stops (caller turbofish tail).
    mod greater_than_at_depth_zero {
      use super::*;

      #[test]
      fn stops_before_gt() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ a > tail }),
          quote! { a }
        );
      }
    }

    /// Whole `Group` following `~` is one token in operand.
    mod group_following_tilde {
      use super::*;

      /// `( )` and `[ ]` groups are call-arg / index expressions — always consumed.
      #[test]
      fn consumes_paren_and_bracket_groups() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ ( y ) }),
          quote! { ( y ) }
        );
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ [ z ] }),
          quote! { [ z ] }
        );
      }

      /// `{ }` brace group directly after `~` (with nothing before it) — stops immediately,
      /// producing an empty operand.  Use `~ ( { x } )` if a block-as-operand is intended.
      #[test]
      fn stops_before_leading_brace_group() {
        assert!(operand_after_tilde_prefix(quote! { ~ { x } }).is_empty());
      }

      /// A call like `~foo() { body }` — the `{ body }` brace group is a new block statement,
      /// not part of the call's argument list.  The operand should be just `foo()`;
      /// the `{ body }` brace group should stay in the surrounding token stream.
      ///
      /// Currently FAILS: `collect_tilde_operand` consumes all `Group` tokens unconditionally,
      /// so `{ body }` is eaten into the operand.
      #[test]
      fn stops_before_brace_group_after_call() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ foo ( ) { body } }),
          quote! { foo ( ) }
        );
      }
    }

    /// Method chain after call remains in operand (`.` not a stop).
    mod method_chain_after_primary {
      use super::*;

      #[test]
      fn includes_dot_calls_in_operand() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ foo ( ) . bar ( ) . baz }),
          quote! { foo ( ) . bar ( ) . baz }
        );
      }
    }

    /// Two separate `>` tokens: operand stops after the first `>` at depth 0 following generics.
    mod double_angle_bracket_limitation {
      use super::*;

      #[test]
      fn operand_boundary_differs_from_rust_parse() {
        assert_ts_eq!(
          operand_after_tilde_prefix(quote! { ~ v :: < u8 > > ( ) }),
          quote! { v :: < u8 > }
        );
      }
    }
  }
}
