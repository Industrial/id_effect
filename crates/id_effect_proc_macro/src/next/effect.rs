//! `effect!` / `effect_local!` (aliases `effect_next!` / `effect_local_next!`): an async body with `~` reads and binds over a typed `R`.
//!
//! - `effect!({ body })` or `effect!(|r: &Caps| { body })` (also `|r|`, `||`). With no
//!   type, `R` comes from the expected type (the fn return type). With `: &Caps`, `R = Caps`. A
//!   named head binds `r: &R` to the environment.
//! - `~Cap` (an upper-case identifier not followed by `::`, `(`, `{` or `!`) reads capability
//!   `Cap`: `<_ as Selector<Cap, _>>::get(Ctx::env(&ctx))`, a `&Arc<Cap>` with the HList index
//!   inferred.
//! - `~expr` (anything else: a path, call, method chain, group) binds a sub-effect:
//!   `Ctx::bind(&ctx, expr).await` (`effect!`) or `__private::bind_local(&ctx, expr).await`
//!   (`effect_local!`). Its error widens into the body's: `Never` to anything, otherwise
//!   through `E: From<E2>` (`__private::Lift`).
//! - `~(a, b, ..)` (a parenthesised list of 2 to 8 effects, `effect!` only) joins them in
//!   parallel on child fibers: `__private::join_binds(&ctx, (branch(&ctx, a).map_err(..), ..))`,
//!   yielding the tuple of values; the first failure interrupts the siblings and is the `~`'s
//!   outcome (`c-join-binds`). `~(expr)` without a top-level comma stays a plain bind.
//! - The body becomes `async move { Ok({ body }) }` owning a `Ctx<R>`; under `effect!` it
//!   must be `Send`.

use proc_macro::{Delimiter, Group, Ident, Spacing, Span, TokenStream, TokenTree};

use super::{compile_error, group, is_path_sep, is_punct, parse, respan, split_commas};

struct Head {
  name: Option<Ident>,
  ty: Option<TokenStream>,
}

/// Splits an optional `|name|` / `|name: &Ty|` / `||` head off the body.
fn split_head(tokens: &[TokenTree]) -> (Head, Vec<TokenTree>) {
  let none = Head {
    name: None,
    ty: None,
  };
  // `||` lexes as one joint `|` `|` pair: both are puncts, handled by the search below.
  if !is_punct(tokens, 0, '|') {
    return (none, unwrap_brace(tokens));
  }
  let Some(close) = (1..tokens.len()).find(|&i| is_punct(tokens, i, '|')) else {
    return (none, unwrap_brace(tokens));
  };
  let inner = &tokens[1..close];
  let body = unwrap_brace(&tokens[close + 1..]);
  let mut head = none;
  if let Some(TokenTree::Ident(id)) = inner.first()
    && id.to_string() != "_"
  {
    head.name = Some(id.clone());
  }
  if let Some(colon) = (0..inner.len()).find(|&i| is_punct(inner, i, ':')) {
    let mut ty = &inner[colon + 1..];
    if is_punct(ty, 0, '&') {
      ty = &ty[1..];
    }
    if matches!(ty.first(), Some(TokenTree::Ident(id)) if id.to_string() == "mut") {
      ty = &ty[1..];
    }
    head.ty = Some(ty.iter().cloned().collect());
  }
  (head, body)
}

fn unwrap_brace(tokens: &[TokenTree]) -> Vec<TokenTree> {
  if let [TokenTree::Group(g)] = tokens
    && g.delimiter() == Delimiter::Brace
  {
    return g.stream().into_iter().collect();
  }
  tokens.to_vec()
}

pub(crate) fn expand(input: TokenStream, local: bool) -> TokenStream {
  let tokens: Vec<TokenTree> = input.into_iter().collect();
  let (head, body) = split_head(&tokens);
  let body = rewrite(&body, local);

  let mut inner =
    parse("#[allow(unused_imports)] use ::id_effect::__private::{LiftFrom as _, LiftNever as _};");
  if let Some(name) = head.name {
    inner.extend(parse("#[allow(unused_variables)] let"));
    inner.extend([TokenTree::Ident(name)]);
    inner.extend(parse("= ::id_effect::Ctx::env(&__ctx);"));
  }
  inner.extend(parse("::core::result::Result::Ok"));
  inner.extend([group(
    Delimiter::Parenthesis,
    group(Delimiter::Brace, body).into(),
  )]);

  let mut closure = parse("move |__ctx: ::id_effect::Ctx<_>| async move");
  closure.extend([group(Delimiter::Brace, inner)]);

  let kind = if local { "LocalEffect" } else { "Effect" };
  let mut out = parse(&format!("::id_effect::{kind}::<_, _,"));
  out.extend(head.ty.unwrap_or_else(|| parse("_")));
  out.extend(parse(">::from_ctx"));
  out.extend([group(Delimiter::Parenthesis, closure)]);
  out
}

fn is_cap_read(t: &[TokenTree], i: usize) -> bool {
  let Some(TokenTree::Ident(id)) = t.get(i) else {
    return false;
  };
  let upper = id
    .to_string()
    .chars()
    .next()
    .is_some_and(char::is_uppercase);
  let followed_by_call = matches!(t.get(i + 1), Some(TokenTree::Group(g)) if matches!(g.delimiter(), Delimiter::Parenthesis | Delimiter::Brace));
  upper && !is_path_sep(t, i + 1) && !followed_by_call && !is_punct(t, i + 1, '!')
}

fn skip_angle(t: &[TokenTree], mut i: usize) -> usize {
  let mut depth = 0i32;
  while i < t.len() {
    if is_punct(t, i, '<') {
      depth += 1;
    } else if is_punct(t, i, '>') {
      depth -= 1;
      if depth == 0 {
        return i + 1;
      }
    }
    i += 1;
  }
  i
}

/// End (exclusive) of the postfix expression starting at `i`: a path (with turbofish and an
/// optional macro call) or a group/literal, then calls, indexing, `?` and `.field` / `.method`.
fn expr_end(t: &[TokenTree], mut i: usize) -> usize {
  match t.get(i) {
    Some(TokenTree::Group(_) | TokenTree::Literal(_)) => i += 1,
    Some(TokenTree::Ident(_) | TokenTree::Punct(_))
      if is_path_sep(t, i) || matches!(t.get(i), Some(TokenTree::Ident(_))) =>
    {
      if is_path_sep(t, i) {
        i += 2;
      }
      loop {
        if matches!(t.get(i), Some(TokenTree::Ident(_))) {
          i += 1;
        }
        if is_path_sep(t, i) {
          i += 2;
          if is_punct(t, i, '<') {
            i = skip_angle(t, i);
            if is_path_sep(t, i) {
              i += 2;
              continue;
            }
            break;
          }
          continue;
        }
        break;
      }
      if is_punct(t, i, '!') && matches!(t.get(i + 1), Some(TokenTree::Group(_))) {
        i += 2;
      }
    }
    _ => return i,
  }
  loop {
    match t.get(i) {
      Some(TokenTree::Group(g))
        if matches!(g.delimiter(), Delimiter::Parenthesis | Delimiter::Bracket) =>
      {
        i += 1;
      }
      Some(TokenTree::Punct(p)) if p.as_char() == '?' => i += 1,
      Some(TokenTree::Punct(p)) if p.as_char() == '.' && p.spacing() == Spacing::Alone => {
        match t.get(i + 1) {
          Some(TokenTree::Ident(_) | TokenTree::Literal(_)) => {
            i += 2;
            if is_path_sep(t, i) && is_punct(t, i + 2, '<') {
              i = skip_angle(t, i + 2);
            }
          }
          _ => break,
        }
      }
      _ => break,
    }
  }
  i
}

/// A nested effect macro (`effect!`, `effect_local!` or their temporary `_next` names) owns its
/// own `~` tokens: its group is copied untouched.
fn is_nested_effect_macro(out: &[TokenTree]) -> bool {
  let n = out.len();
  n >= 2
    && matches!(&out[n - 1], TokenTree::Punct(p) if p.as_char() == '!')
    && matches!(&out[n - 2], TokenTree::Ident(id) if matches!(id.to_string().as_str(), "effect" | "effect_local" | "effect_next" | "effect_local_next"))
}

/// The most effects one `~(a, b, ..)` joins (`__private::JoinTuple` impls).
const MAX_JOIN: usize = 8;

/// The branches of a parallel join `~(a, b, ..)`: a parenthesised group with at least two
/// comma-separated items at its top level (`~(expr)` stays a plain bind).
fn join_branches(t: &TokenTree) -> Option<Vec<Vec<TokenTree>>> {
  let TokenTree::Group(g) = t else {
    return None;
  };
  if g.delimiter() != Delimiter::Parenthesis {
    return None;
  }
  let items = split_commas(g.stream().into_iter().collect());
  (items.len() >= 2).then_some(items)
}

/// `~(a, b, ..)` in `effect!`:
/// `match __private::join_binds(&__ctx, (branch(&__ctx, a).map_err(lift), ..)).await { .. }`.
/// Each branch's error widens into the body's on its own (as `~a` would), so the joined effect
/// has the body's error and its `Err` is returned as is.
fn expand_join(branches: Vec<Vec<TokenTree>>, span: Span, local: bool) -> TokenStream {
  if local {
    return compile_error(
      "a parallel join `~(a, b, ..)` needs `effect!` (its branches run on child fibers); bind them one at a time in `effect_local!`",
      span,
    );
  }
  if branches.len() > MAX_JOIN {
    return compile_error(
      "`~(..)` joins at most 8 effects; nest a join or use `all`",
      span,
    );
  }
  let mut tuple = TokenStream::new();
  for b in branches {
    let bspan = b[0].span();
    let mut args = respan(parse("&__ctx,"), bspan);
    args.extend(rewrite(&b, local));
    tuple.extend(respan(parse("::id_effect::__private::branch"), bspan));
    tuple.extend([group(Delimiter::Parenthesis, args)]);
    tuple.extend(respan(
      parse(".map_err(|__e| (&&::id_effect::__private::Lift::new(__e)).lift()),"),
      bspan,
    ));
  }
  let mut args = respan(parse("&__ctx,"), span);
  args.extend([group(Delimiter::Parenthesis, tuple)]);
  let mut m = respan(parse("match ::id_effect::__private::join_binds"), span);
  m.extend([group(Delimiter::Parenthesis, args)]);
  m.extend(respan(parse(".await"), span));
  m.extend([group(
    Delimiter::Brace,
    respan(
      parse(
        "::core::result::Result::Ok(__v) => __v, \
         ::core::result::Result::Err(__e) => return ::core::result::Result::Err(__e),",
      ),
      span,
    ),
  )]);
  group(Delimiter::Parenthesis, m).into()
}

fn rewrite(tokens: &[TokenTree], local: bool) -> TokenStream {
  let mut out: Vec<TokenTree> = Vec::with_capacity(tokens.len());
  let mut i = 0;
  while i < tokens.len() {
    match &tokens[i] {
      TokenTree::Punct(p) if p.as_char() == '~' => {
        let start = i + 1;
        if is_cap_read(tokens, start) {
          let TokenTree::Ident(id) = &tokens[start] else {
            unreachable!("is_cap_read checked for an ident")
          };
          // A direct trait call (not `Ctx::get`): a missing capability then reports only the
          // `Selector` diagnostic, without a `required by a bound in Ctx::get` note.
          let mut call = parse("<_ as ::id_effect::Selector<");
          call.extend([TokenTree::Ident(id.clone())]);
          call.extend(parse(", _>>::get(::id_effect::Ctx::env(&__ctx))"));
          out.push(group(Delimiter::Parenthesis, respan(call, id.span())));
          i = start + 1;
          continue;
        }
        let end = expr_end(tokens, start);
        if end == start + 1
          && let Some(branches) = join_branches(&tokens[start])
        {
          out.extend(expand_join(branches, tokens[start].span(), local));
          i = end;
          continue;
        }
        if end == start {
          out.extend(compile_error(
            "`~` must be followed by a capability type (`~Db`) or an effect expression (`~count_users()`)",
            p.span(),
          ));
          i += 1;
          continue;
        }
        let span = tokens[start].span();
        let expr = rewrite(&tokens[start..end], local);
        let bind = if local {
          "::id_effect::__private::bind_local"
        } else {
          "::id_effect::Ctx::bind"
        };
        let mut call = parse(bind);
        let mut args = parse("&__ctx,");
        args.extend(expr);
        call.extend([group(Delimiter::Parenthesis, args)]);
        let call = respan(call, span);
        let mut m = parse("match");
        m.extend(call);
        m.extend(respan(parse(".await"), span));
        m.extend([group(
          Delimiter::Brace,
          respan(
            parse(
              "::core::result::Result::Ok(__v) => __v, \
               ::core::result::Result::Err(__e) => return ::core::result::Result::Err(\
               (&&::id_effect::__private::Lift::new(__e)).lift()),",
            ),
            span,
          ),
        )]);
        out.push(group(Delimiter::Parenthesis, m));
        i = end;
      }
      TokenTree::Group(g) => {
        if is_nested_effect_macro(&out) {
          out.push(TokenTree::Group(g.clone()));
        } else {
          let inner: Vec<TokenTree> = g.stream().into_iter().collect();
          let mut ng = Group::new(g.delimiter(), rewrite(&inner, local));
          ng.set_span(g.span());
          out.push(TokenTree::Group(ng));
        }
        i += 1;
      }
      other => {
        out.push(other.clone());
        i += 1;
      }
    }
  }
  out.into_iter().collect()
}
