//! The temporary next-kernel macros (`effect_next!`, `effect_local_next!`, `caps_next!`), ported
//! from spike A's token-level macros (no `syn`). They coexist with the v1 `effect!` and are
//! renamed by `k-retire-sweep`. Expansions only name `::id_effect::next::…` paths (the `id_effect`
//! crate has `extern crate self as id_effect`, so they also resolve inside it).

pub(crate) mod caps;
pub(crate) mod effect;

use proc_macro::{Delimiter, Group, Ident, Literal, Punct, Spacing, Span, TokenStream, TokenTree};
use std::str::FromStr;

/// Parses an internal token template (always valid Rust tokens).
fn parse(s: &str) -> TokenStream {
  TokenStream::from_str(s).unwrap_or_else(|_| {
    compile_error(
      "id_effect_proc_macro: invalid internal token template",
      Span::call_site(),
    )
  })
}

/// `compile_error!("msg")` spanned at `span`.
fn compile_error(msg: &str, span: Span) -> TokenStream {
  let ident = Ident::new("compile_error", span);
  let mut bang = Punct::new('!', Spacing::Alone);
  bang.set_span(span);
  let mut lit = Literal::string(msg);
  lit.set_span(span);
  let mut group = Group::new(Delimiter::Parenthesis, TokenTree::Literal(lit).into());
  group.set_span(span);
  [
    TokenTree::Ident(ident),
    TokenTree::Punct(bang),
    TokenTree::Group(group),
  ]
  .into_iter()
  .collect()
}

/// Gives every token of `ts` (recursively) the span `span`, so errors in generated code point at
/// the user's tokens.
fn respan(ts: TokenStream, span: Span) -> TokenStream {
  ts.into_iter()
    .map(|tt| match tt {
      TokenTree::Group(g) => {
        let mut ng = Group::new(g.delimiter(), respan(g.stream(), span));
        ng.set_span(span);
        TokenTree::Group(ng)
      }
      mut other => {
        other.set_span(span);
        other
      }
    })
    .collect()
}

fn group(delim: Delimiter, ts: TokenStream) -> TokenTree {
  TokenTree::Group(Group::new(delim, ts))
}

fn is_punct(t: &[TokenTree], i: usize, c: char) -> bool {
  matches!(t.get(i), Some(TokenTree::Punct(p)) if p.as_char() == c)
}

fn is_path_sep(t: &[TokenTree], i: usize) -> bool {
  matches!(t.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ':' && p.spacing() == Spacing::Joint)
    && is_punct(t, i + 1, ':')
}

/// Splits a comma-separated list at `<..>` depth 0 (delimited groups are single tokens already).
fn split_commas(tokens: Vec<TokenTree>) -> Vec<Vec<TokenTree>> {
  let mut items = Vec::new();
  let mut cur: Vec<TokenTree> = Vec::new();
  let mut depth = 0i32;
  for tt in tokens {
    if let TokenTree::Punct(p) = &tt {
      let after_dash = matches!(cur.last(), Some(TokenTree::Punct(q)) if q.as_char() == '-');
      match p.as_char() {
        '<' => depth += 1,
        '>' if !after_dash => depth -= 1,
        ',' if depth == 0 => {
          if !cur.is_empty() {
            items.push(std::mem::take(&mut cur));
          }
          continue;
        }
        _ => {}
      }
    }
    cur.push(tt);
  }
  if !cur.is_empty() {
    items.push(cur);
  }
  items
}
