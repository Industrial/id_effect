//! `caps!(A, B, C)` → `HCons<A, HCons<B, HCons<C, HNil>>>`, rejecting duplicates by token.
//!
//! A capability listed twice makes every later lookup of it ambiguous (E0283), and
//! `macro_rules!` cannot compare idents, hence a proc-macro. Two items are the same capability
//! when their tokens print the same (`Db` and `crate::Db` are different spellings and are not
//! caught; the type-level `Selector` ambiguity still is).
//!
//! On a duplicate the macro reports one `compile_error!` at each repeated occurrence **and still
//! emits the de-duplicated list**, through the identity projection
//! `<List as __private::CapsChecked<{ compile_error!(..); 0 }>>::Out`. The prototype returned
//! only the `compile_error!`, whose type position became `()`, so every `~Cap` read of the
//! effect then cascaded into a second `Selector<Cap, _> is not implemented for ()` error.

use proc_macro::{Delimiter, Span, TokenStream, TokenTree};

use super::{compile_error, group, parse, split_commas};

pub(crate) fn expand(input: TokenStream) -> TokenStream {
  let items = split_commas(input.into_iter().collect());
  let mut seen: Vec<String> = Vec::new();
  let mut unique: Vec<Vec<TokenTree>> = Vec::new();
  let mut errors = TokenStream::new();
  for item in items {
    let key = item
      .iter()
      .map(ToString::to_string)
      .collect::<Vec<_>>()
      .join(" ");
    if seen.contains(&key) {
      let span = item.first().map_or_else(Span::call_site, TokenTree::span);
      errors.extend(compile_error(
        &format!(
          "capability `{key}` appears twice in `caps!(..)`; an environment holds each \
           capability once"
        ),
        span,
      ));
      errors.extend(parse(";"));
      continue;
    }
    seen.push(key);
    unique.push(item);
  }

  let mut acc = parse("::id_effect::HNil");
  for item in unique.into_iter().rev() {
    let mut ts = parse("::id_effect::HCons<");
    ts.extend(item);
    ts.extend(parse(","));
    ts.extend(acc);
    ts.extend(parse(">"));
    acc = ts;
  }
  if errors.is_empty() {
    return acc;
  }

  // `<acc as CapsChecked<{ errors; 0 }>>::Out` (= `acc`): the errors, and the list for the rest
  // of type checking.
  let mut block = errors;
  block.extend(parse("0usize"));
  let mut out = parse("<");
  out.extend(acc);
  out.extend(parse("as ::id_effect::__private::CapsChecked<"));
  out.extend([group(Delimiter::Brace, block)]);
  out.extend(parse(">>::Out"));
  out
}
