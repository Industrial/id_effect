//! Expansion of `#[service(version = N)]` (used as `#[rpc::service]`): a trait of procedures
//! becomes the handler trait, `<Trait>Service` (contract, registration, mount) and
//! `<Trait>Client<T>` (the Rust client over any transport).

use std::collections::BTreeMap;

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
  Attribute, Expr, ExprLit, FnArg, Ident, ItemTrait, Lit, LitInt, LitStr, MetaNameValue, Pat,
  ReturnType, Token, TraitItem, TraitItemFn, Type,
};

use crate::support::{docs, error, is_valid_name, rt, segment, wire_schema};

/// The six procedure kinds, by attribute name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
  Query,
  Mutation,
  ServerStream,
  ClientStream,
  Bidi,
  Subscription,
}

const KINDS: [(&str, Kind); 6] = [
  ("query", Kind::Query),
  ("mutation", Kind::Mutation),
  ("server_stream", Kind::ServerStream),
  ("client_stream", Kind::ClientStream),
  ("bidi", Kind::Bidi),
  ("subscription", Kind::Subscription),
];

const KIND_LIST: &str = "`#[query]`, `#[mutation]`, `#[server_stream]`, `#[client_stream]`, `#[bidi]` or `#[subscription]`";

impl Kind {
  fn attr(self) -> &'static str {
    match self {
      Self::Query => "query",
      Self::Mutation => "mutation",
      Self::ServerStream => "server_stream",
      Self::ClientStream => "client_stream",
      Self::Bidi => "bidi",
      Self::Subscription => "subscription",
    }
  }

  fn ident(self) -> Ident {
    Ident::new(self.attr(), Span::call_site())
  }

  fn contract_kind(self) -> TokenStream {
    let variant = match self {
      Self::Query => quote!(Query),
      Self::Mutation => quote!(Mutation),
      Self::ServerStream => quote!(ServerStream),
      Self::ClientStream => quote!(ClientStream),
      Self::Bidi => quote!(Bidi),
      Self::Subscription => quote!(Subscription),
    };
    quote!(::id_effect_rpc::contract::ProcedureKind::#variant)
  }
}

/// `version = N`, `name = "…"`.
struct Args {
  version: u32,
  name: Option<String>,
}

fn parse_args(attr: TokenStream) -> syn::Result<Args> {
  let span = if attr.is_empty() {
    Span::call_site()
  } else {
    attr.span()
  };
  let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated
    .parse2(attr)
    .map_err(|e| {
      error(
        e.span(),
        format!("cannot read the #[rpc::service] arguments: {e}"),
        "write `#[rpc::service(version = 1)]`, optionally with `name = \"Service\"`",
      )
    })?;
  let mut version = None;
  let mut name = None;
  for pair in pairs {
    let lit = match &pair.value {
      Expr::Lit(ExprLit { lit, .. }) => Some(lit),
      _ => None,
    };
    if pair.path.is_ident("version") && version.is_none() {
      let Some(Lit::Int(n)) = lit else {
        return Err(error(
          pair.value.span(),
          "the service version is a number",
          "write `version = 1`",
        ));
      };
      version = Some(parse_version(n)?);
    } else if pair.path.is_ident("name") && name.is_none() {
      let Some(Lit::Str(s)) = lit else {
        return Err(error(
          pair.value.span(),
          "the service name is a string",
          "write `name = \"Greeter\"`",
        ));
      };
      name = Some(service_name(s)?);
    } else {
      return Err(error(
        pair.path.span(),
        "unknown or repeated #[rpc::service] argument",
        "#[rpc::service] takes `version = N` once and optionally `name = \"Service\"` once",
      ));
    }
  }
  let Some(version) = version else {
    return Err(error(
      span,
      "#[rpc::service] needs a contract version",
      "write `#[rpc::service(version = 1)]`; versions start at 1 and are served side by side",
    ));
  };
  Ok(Args { version, name })
}

fn parse_version(n: &LitInt) -> syn::Result<u32> {
  match n.base10_parse::<u32>() {
    Ok(v) if v >= 1 => Ok(v),
    _ => Err(error(
      n.span(),
      format!("`{n}` is not a service version"),
      "versions are whole numbers from 1: `version = 1`",
    )),
  }
}

fn service_name(s: &LitStr) -> syn::Result<String> {
  let name = s.value();
  if is_valid_name(&name) {
    Ok(name)
  } else {
    Err(error(
      s.span(),
      format!("`{name}` is not a service name"),
      "use 1 to 64 characters of [A-Za-z0-9_-]; the tag is `<Service>.<procedure>`",
    ))
  }
}

/// A procedure's access decision.
enum Access {
  Public,
  Guards(Vec<Expr>),
}

/// One parsed procedure.
struct Procedure {
  method: Ident,
  name: String,
  kind: Kind,
  access: Access,
  max_items: Option<LitInt>,
  max_bytes: Option<LitInt>,
  docs: Option<String>,
  /// The declared return type (`Effect<A, E, R>`).
  ret: Type,
  /// `E`.
  error: Type,
  /// The input parameter's type, if the method takes one.
  input: Option<Type>,
  /// The type of one input item (or the input).
  in_item: Type,
  /// The type of one output item (or the output).
  out_item: Type,
  /// A subscription method takes the resume cursor as its second parameter.
  cursor: bool,
}

/// The attributes of one method that belong to this macro.
#[derive(Default)]
struct MethodAttrs {
  kind: Option<Kind>,
  name: Option<String>,
  max_items: Option<LitInt>,
  max_bytes: Option<LitInt>,
  public: bool,
  guards: Vec<Expr>,
}

fn kind_of(attr: &Attribute) -> Option<Kind> {
  KINDS
    .iter()
    .find(|(name, _)| attr.path().is_ident(name))
    .map(|(_, kind)| *kind)
}

fn is_ours(attr: &Attribute) -> bool {
  kind_of(attr).is_some() || attr.path().is_ident("public") || attr.path().is_ident("guard")
}

fn kind_options(attr: &Attribute, kind: Kind, out: &mut MethodAttrs) -> syn::Result<()> {
  if !matches!(attr.meta, syn::Meta::List(_)) {
    return Ok(());
  }
  attr.parse_nested_meta(|meta| {
    if meta.path.is_ident("name") {
      let lit = meta.value()?.parse::<LitStr>()?;
      let name = lit.value();
      if !is_valid_name(&name) {
        return Err(error(
          lit.span(),
          format!("`{name}` is not a procedure name"),
          "use 1 to 64 characters of [A-Za-z0-9_-]",
        ));
      }
      out.name = Some(name);
    } else if meta.path.is_ident("max_items") {
      out.max_items = Some(meta.value()?.parse::<LitInt>()?);
    } else if meta.path.is_ident("max_bytes") {
      out.max_bytes = Some(meta.value()?.parse::<LitInt>()?);
    } else {
      return Err(error(
        meta.path.span(),
        "unknown procedure option",
        format!(
          "`#[{}(..)]` takes `name = \"…\"`, `max_items = N` and `max_bytes = N`",
          kind.attr()
        ),
      ));
    }
    Ok(())
  })
}

fn method_attrs(method: &TraitItemFn) -> syn::Result<MethodAttrs> {
  let mut out = MethodAttrs::default();
  let ident = &method.sig.ident;
  for attr in &method.attrs {
    if let Some(kind) = kind_of(attr) {
      if let Some(first) = out.kind {
        return Err(error(
          attr.span(),
          format!(
            "procedure `{ident}` has two kinds (`#[{}]` and `#[{}]`)",
            first.attr(),
            kind.attr()
          ),
          "keep one: a procedure has exactly one kind",
        ));
      }
      out.kind = Some(kind);
      kind_options(attr, kind, &mut out)?;
    } else if attr.path().is_ident("public") {
      if out.public || !out.guards.is_empty() {
        return Err(conflicting_access(attr.span(), ident));
      }
      attr.meta.require_path_only()?;
      out.public = true;
    } else if attr.path().is_ident("guard") {
      if out.public {
        return Err(conflicting_access(attr.span(), ident));
      }
      let guard = attr.parse_args::<Expr>().map_err(|e| {
        error(
          e.span(),
          "`#[guard(..)]` takes the guard as an expression",
          "write `#[guard(RequireRole::new(\"staff\"))]` (any `RouteGuard`; repeat it to stack guards, outermost last)",
        )
      })?;
      out.guards.push(guard);
    }
  }
  Ok(out)
}

fn conflicting_access(span: Span, ident: &Ident) -> syn::Error {
  error(
    span,
    format!("procedure `{ident}` has conflicting access decisions"),
    "use `#[public]` alone, or one or more `#[guard(G)]` without `#[public]`",
  )
}

/// `Name<A, B, ..>` → `[A, B, ..]` when the last segment is `name` with at least `n` type args.
fn args_of<'t>(ty: &'t Type, name: &str, n: usize) -> Option<Vec<&'t Type>> {
  segment(ty).and_then(|(seg, args)| (seg == name && args.len() >= n).then_some(args))
}

fn effect_return(span: Span, ident: &Ident) -> syn::Error {
  error(
    span,
    format!("procedure `{ident}` does not return an `Effect`"),
    "return `Effect<Output, Error, DynEnv>` (`id_effect::Never` as the error when it cannot fail)",
  )
}

fn too_many(span: Span, ident: &Ident, takes: &str) -> syn::Error {
  error(
    span,
    format!("procedure `{ident}` takes too many parameters"),
    format!("{takes}; put several values in one `#[derive(Schema)]` struct"),
  )
}

/// The parameter types after `&self`.
fn parameters(method: &TraitItemFn) -> syn::Result<Vec<Type>> {
  let sig = &method.sig;
  let ident = &sig.ident;
  let mut inputs = sig.inputs.iter();
  match inputs.next() {
    Some(FnArg::Receiver(r)) if r.reference.is_some() && r.mutability.is_none() => {}
    _ => {
      return Err(error(
        ident.span(),
        format!("procedure `{ident}` does not take `&self`"),
        "write `fn name(&self, input: I) -> Effect<O, E, DynEnv>`",
      ));
    }
  }
  inputs
    .map(|arg| match arg {
      FnArg::Typed(pat) if matches!(*pat.pat, Pat::Ident(_) | Pat::Wild(_)) => {
        Ok((*pat.ty).clone())
      }
      other => Err(error(
        other.span(),
        "procedure parameters are plain names",
        "bind the input to a name, e.g. `input: GreetIn`, and destructure it in the body",
      )),
    })
    .collect()
}

/// `(input, in_item, out_item, cursor)` of a procedure of `kind` taking `params` and returning
/// `Effect<ok, _, _>`.
fn shape(
  kind: Kind,
  ident: &Ident,
  mut params: Vec<Type>,
  ok: &Type,
  inputs_span: Span,
) -> syn::Result<(Option<Type>, Type, Type, bool)> {
  let unit: Type = syn::parse_quote!(());
  match kind {
    Kind::Query | Kind::Mutation | Kind::ServerStream => {
      if params.len() > 1 {
        return Err(too_many(
          params[1].span(),
          ident,
          "a query, mutation or server stream takes one input",
        ));
      }
      let input = params.pop();
      let in_item = input.clone().unwrap_or(unit);
      let out_item = if kind == Kind::ServerStream {
        let Some(item) = args_of(ok, "Stream", 1) else {
          return Err(error(
            ok.span(),
            format!("`#[server_stream]` procedure `{ident}` does not return a `Stream`"),
            "return `Effect<Stream<Item, E, DynEnv>, E, DynEnv>`",
          ));
        };
        item[0].clone()
      } else {
        ok.clone()
      };
      Ok((input, in_item, out_item, false))
    }
    Kind::ClientStream => {
      let item = match params.as_slice() {
        [ty] => args_of(ty, "Stream", 1).map(|args| args[0].clone()),
        _ => None,
      };
      let Some(item) = item else {
        return Err(error(
          inputs_span,
          format!("`#[client_stream]` procedure `{ident}` does not take a `Stream`"),
          "take the client's items as its only parameter: `items: Stream<Item, E, DynEnv>`",
        ));
      };
      Ok((params.pop(), item, ok.clone(), false))
    }
    Kind::Bidi => {
      if !params.is_empty() {
        return Err(too_many(
          params[0].span(),
          ident,
          "a `#[bidi]` procedure takes no parameters; the client's items reach its `Duplex`",
        ));
      }
      let Some(duplex) = args_of(ok, "Duplex", 2) else {
        return Err(error(
          ok.span(),
          format!("`#[bidi]` procedure `{ident}` does not return a `Duplex`"),
          "return `Effect<Duplex<In, Out, E>, E, DynEnv>`; `Duplex::new(|items| …)` maps the client's items to the server's",
        ));
      };
      Ok((None, duplex[0].clone(), duplex[1].clone(), false))
    }
    Kind::Subscription => {
      if params.len() > 2 {
        return Err(too_many(
          params[2].span(),
          ident,
          "a subscription takes one input and optionally the resume cursor `after: Option<Cursor>`",
        ));
      }
      let Some(item) = args_of(ok, "Subscription", 1) else {
        return Err(error(
          ok.span(),
          format!("`#[subscription]` procedure `{ident}` does not return a `Subscription`"),
          "return `Effect<Subscription<Item, E>, E, DynEnv>`",
        ));
      };
      let cursor = params.len() == 2;
      let input = params.into_iter().next();
      let in_item = input.clone().unwrap_or(unit);
      Ok((input, in_item, item[0].clone(), cursor))
    }
  }
}

fn procedure(method: &TraitItemFn) -> syn::Result<Procedure> {
  let sig = &method.sig;
  let ident = &sig.ident;
  let attrs = method_attrs(method)?;
  let Some(kind) = attrs.kind else {
    return Err(error(
      ident.span(),
      format!("method `{ident}` has no procedure kind"),
      format!("mark it with one of {KIND_LIST}"),
    ));
  };
  let access = if attrs.public {
    Access::Public
  } else if attrs.guards.is_empty() {
    return Err(error(
      ident.span(),
      format!("procedure `{ident}` has no access decision"),
      "add `#[public]` to let anyone call it, or `#[guard(G)]` to run a guard first; procedures deny by default (ADR 0013 R1)",
    ));
  } else {
    Access::Guards(attrs.guards)
  };
  if let Some(token) = sig.asyncness {
    return Err(error(
      token.span(),
      format!("procedure `{ident}` is `async`"),
      "return an `Effect<A, E, DynEnv>` instead; the effect may await inside",
    ));
  }
  if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
    return Err(error(
      sig.generics.span(),
      format!("procedure `{ident}` is generic"),
      "use concrete types: they are part of the service contract",
    ));
  }
  let params = parameters(method)?;
  let ReturnType::Type(_, ret) = &sig.output else {
    return Err(effect_return(ident.span(), ident));
  };
  let ret = (**ret).clone();
  let Some(effect) = args_of(&ret, "Effect", 2) else {
    return Err(effect_return(ret.span(), ident));
  };
  let (ok, err) = (effect[0].clone(), effect[1].clone());
  let (input, in_item, out_item, cursor) = shape(kind, ident, params, &ok, sig.inputs.span())?;
  Ok(Procedure {
    method: ident.clone(),
    name: attrs.name.unwrap_or_else(|| ident.to_string()),
    kind,
    access,
    max_items: attrs.max_items,
    max_bytes: attrs.max_bytes,
    docs: docs(&method.attrs),
    ret,
    error: err,
    input,
    in_item,
    out_item,
    cursor,
  })
}

/// The trait with this macro's attributes removed, and its procedures.
fn parse_trait(item: TokenStream) -> syn::Result<(ItemTrait, Vec<Procedure>)> {
  let mut service: ItemTrait = syn::parse2(item).map_err(|err| {
    error(
      err.span(),
      "#[rpc::service] goes on a trait",
      "declare the procedures as trait methods: `trait Greeter { #[query] #[public] fn greet(&self, name: String) -> Effect<String, Never, DynEnv>; }`",
    )
  })?;
  if !service.generics.params.is_empty() || service.generics.where_clause.is_some() {
    return Err(error(
      service.generics.span(),
      "an rpc service trait cannot be generic",
      "use concrete types in the procedures: they are part of the service contract",
    ));
  }
  let mut procedures = Vec::new();
  let mut names: BTreeMap<String, ()> = BTreeMap::new();
  for item in &mut service.items {
    let TraitItem::Fn(method) = item else {
      return Err(error(
        item.span(),
        "only procedures belong in an rpc service trait",
        "move constants, types and macros out of the trait; every item is a procedure method",
      ));
    };
    let procedure = procedure(method)?;
    if names.insert(procedure.name.clone(), ()).is_some() {
      return Err(error(
        method.sig.ident.span(),
        format!("two procedures are named `{}`", procedure.name),
        format!(
          "procedure names are unique within a service; rename one with `#[{}(name = \"…\")]`",
          procedure.kind.attr()
        ),
      ));
    }
    method.attrs.retain(|attr| !is_ours(attr));
    procedures.push(procedure);
  }
  Ok((service, procedures))
}

/// The handler closure of `p`: calls `handler.<method>` (`real`) or dies (the contract's
/// registration, which is never called).
fn handler_closure(p: &Procedure, real: bool) -> TokenStream {
  let p_ = quote!(::id_effect_rpc::service::__private);
  let ret = &p.ret;
  let method = &p.method;
  let env = quote!(_env: &mut #p_::DynEnv);
  let cursor_ty = quote!(::core::option::Option<::id_effect_rpc::wire::Cursor>);
  let takes_input = real && p.input.is_some();
  let input = if takes_input {
    quote!(input)
  } else {
    quote!(_)
  };
  let args = if takes_input { quote!(input) } else { quote!() };
  let call = |args: TokenStream| {
    if real {
      quote!(handler.#method(#args))
    } else {
      quote!(#p_::die("contract registration: never called"))
    }
  };
  let ty = &p.in_item;
  let closure = match p.kind {
    Kind::Bidi => {
      let call = call(quote!());
      quote!(move |#env| -> #ret { #call })
    }
    Kind::ClientStream => {
      let ty = p.input.as_ref().unwrap_or(&p.in_item);
      let call = call(args);
      quote!(move |#input: #ty, #env| -> #ret { #call })
    }
    Kind::Subscription => {
      let with_cursor = takes_input && p.cursor;
      let after = if with_cursor {
        quote!(after)
      } else {
        quote!(_)
      };
      let call = call(if with_cursor {
        quote!(input, after)
      } else {
        args
      });
      quote!(move |#input: #ty, #after: #cursor_ty, #env| -> #ret { #call })
    }
    Kind::Query | Kind::Mutation | Kind::ServerStream => {
      let call = call(args);
      quote!(move |#input: #ty, #env| -> #ret { #call })
    }
  };
  if real {
    quote!({
      let handler = ::std::sync::Arc::clone(&handler);
      #closure
    })
  } else {
    closure
  }
}

/// `schema.with_serde()` when its type implements serde `Serialize` + `Deserialize` (so the
/// procedure is served over postcard), `schema` otherwise: autoref specialization, decided by the
/// compiler for the concrete type.
fn serde_bridge(schema: TokenStream) -> TokenStream {
  let p_ = quote!(::id_effect_rpc::service::__private);
  quote!({
    #[allow(unused_imports)]
    use #p_::{WithSerde as _, WithoutSerde as _};
    (&#p_::SerdeProbe(#schema)).wire_schema()
  })
}

/// `group.<kind>(Procedure::<kind>(tag)…, in, out, handler);`
fn registration(service: &str, p: &Procedure, real: bool) -> TokenStream {
  let tag = format!("{service}.{}", p.name);
  let ctor = p.kind.ident();
  let mut procedure = quote!(::id_effect_rpc::Procedure::#ctor(#tag).version(Self::VERSION));
  if let Some(doc) = &p.docs {
    procedure = quote!(#procedure.describe(#doc));
  }
  if let Some(n) = &p.max_items {
    procedure = quote!(#procedure.max_items(#n));
  }
  if let Some(n) = &p.max_bytes {
    procedure = quote!(#procedure.max_bytes(#n));
  }
  procedure = match &p.access {
    Access::Public => quote!(#procedure.public()),
    Access::Guards(guards) => quote!(#procedure #(.guard(#guards))*),
  };
  let (mut input, mut output) = (wire_schema(&p.in_item), wire_schema(&p.out_item));
  if real {
    input = serde_bridge(input);
    output = serde_bridge(output);
  }
  let handler = handler_closure(p, real);
  quote!(group.#ctor(#procedure, #input, #output, #handler);)
}

/// The client method of `p`.
fn client_method(service: &str, p: &Procedure) -> TokenStream {
  let rt = rt();
  let tag = format!("{service}.{}", p.name);
  let method = &p.method;
  let err = &p.error;
  let (in_item, out_item) = (&p.in_item, &p.out_item);
  let (input, output) = (wire_schema(in_item), wire_schema(out_item));
  let call_err = quote!(#rt::RpcCallError<#err>);
  let effect = quote!(#rt::__private::Effect);
  let stream = quote!(#rt::__private::Stream);
  let doc = format!(
    "Call `{tag}` ({}).{}",
    p.kind.attr(),
    p.docs
      .as_ref()
      .map(|d| format!("\n\n{d}"))
      .unwrap_or_default()
  );
  let r = quote!(R: ::core::marker::Send + ::core::marker::Sync + 'static);
  let (param, arg) = match &p.input {
    Some(_) => (quote!(input: #in_item,), quote!(input)),
    None => (quote!(), quote!(())),
  };
  let body = match p.kind {
    Kind::Query | Kind::Mutation => {
      let kind = p.kind.contract_kind();
      quote! {
        pub fn #method<#r>(&self, #param) -> #effect<#out_item, #call_err, R> {
          #rt::call_unary(&self.transport, #kind, #tag, Self::VERSION, #arg, #input, #output)
        }
      }
    }
    Kind::ServerStream => quote! {
      pub fn #method<#r>(&self, #param) -> #effect<#stream<#out_item, #call_err, R>, #call_err, R> {
        #rt::call_server_stream(&self.transport, #tag, Self::VERSION, #arg, #input, #output)
      }
    },
    Kind::ClientStream => quote! {
      pub fn #method<#r>(&self, items: ::std::vec::Vec<#in_item>) -> #effect<#out_item, #call_err, R> {
        #rt::call_client_stream(&self.transport, #tag, Self::VERSION, items, #input, #output)
      }
    },
    Kind::Bidi => quote! {
      pub fn #method<#r>(&self) -> #effect<(#rt::BidiSender<#in_item, #err>, #stream<#out_item, #call_err, R>), #call_err, R> {
        #rt::call_bidi(&self.transport, #tag, Self::VERSION, #input, #output)
      }
    },
    Kind::Subscription => {
      let item = quote!(::id_effect_rpc::kinds::SubscriptionItem<#out_item>);
      quote! {
        pub fn #method<#r>(
          &self,
          #param
          after: ::core::option::Option<::id_effect_rpc::wire::Cursor>,
        ) -> #effect<#stream<#item, #call_err, R>, #call_err, R> {
          #rt::call_subscribe(&self.transport, #tag, Self::VERSION, #arg, after, #input, #output)
        }
      }
    }
  };
  quote! {
    #[doc = #doc]
    #[allow(clippy::type_complexity)]
    #body
  }
}

/// Parses `#[service(version = N)]` on a trait and emits the handler trait, `<Trait>Service`
/// and `<Trait>Client<T>`.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
  let args = parse_args(attr)?;
  let (service, procedures) = parse_trait(item)?;
  let rt = rt();
  let p_ = quote!(#rt::__private);
  let ident = &service.ident;
  let vis = &service.vis;
  let name = args.name.unwrap_or_else(|| ident.to_string());
  if !is_valid_name(&name) {
    return Err(error(
      ident.span(),
      format!("`{name}` is not a service name"),
      "rename the trait or set `name = \"…\"` (1 to 64 characters of [A-Za-z0-9_-])",
    ));
  }
  let version = args.version;
  let service_ty = format_ident!("{}Service", ident);
  let client_ty = format_ident!("{}Client", ident);
  let description = docs(&service.attrs).map(|d| quote!(.with_description(#d)));

  let checks = procedures.iter().map(|p| {
    let (input, output) = (wire_schema(&p.in_item), wire_schema(&p.out_item));
    let err = &p.error;
    let assert = quote_spanned!(err.span()=> #p_::assert_error::<#err>(););
    quote! {
      let _ = #input;
      let _ = #output;
      #assert
    }
  });
  let real = procedures.iter().map(|p| registration(&name, p, true));
  let contract_only = procedures.iter().map(|p| registration(&name, p, false));
  let methods = procedures.iter().map(|p| client_method(&name, p));
  let mutable = (!procedures.is_empty()).then(|| quote!(mut));

  let service_doc =
    format!("The `{name}` rpc service, version {version} (`#[rpc::service]` on [`{ident}`]).");
  let client_doc = format!(
    "The Rust client of the `{name}` rpc service, version {version}, over the transport `T`."
  );

  Ok(quote! {
    #service

    // Every procedure type crosses the wire and every error type is a union; checked here
    // first, so a missing derive is the first error.
    const _: () = {
      #[allow(dead_code)]
      fn __rpc_service_checks() {
        #(#checks)*
      }
    };

    #[doc = #service_doc]
    #[derive(::core::clone::Clone, ::core::marker::Copy, ::core::fmt::Debug, ::core::default::Default)]
    #vis struct #service_ty;

    impl #service_ty {
      /// The service name.
      pub const NAME: &'static str = #name;
      /// The contract version.
      pub const VERSION: u32 = #version;

      /// This version's contract.
      pub fn contract() -> ::id_effect_rpc::ServiceContract {
        let #mutable group = ::id_effect_rpc::RpcGroup::new();
        #(#contract_only)*
        group.service_contract(Self::NAME, Self::VERSION) #description
      }

      /// Register every procedure of `handler` in `group`.
      ///
      /// # Panics
      ///
      /// When this service version is already registered in `group`.
      pub fn register<S>(group: &mut ::id_effect_rpc::RpcGroup, handler: S)
      where
        S: #ident + ::core::marker::Send + ::core::marker::Sync + 'static,
      {
        let handler = ::std::sync::Arc::new(handler);
        #(#real)*
        let _ = (group, handler);
      }

      /// A group serving `handler`.
      pub fn group<S>(handler: S) -> ::id_effect_rpc::RpcGroup
      where
        S: #ident + ::core::marker::Send + ::core::marker::Sync + 'static,
      {
        let mut group = ::id_effect_rpc::RpcGroup::new();
        Self::register(&mut group, handler);
        group
      }

      /// An `RpcServer` serving `handler` under `/rpc`.
      pub fn server<S>(handler: S) -> ::id_effect_rpc::RpcServer
      where
        S: #ident + ::core::marker::Send + ::core::marker::Sync + 'static,
      {
        ::id_effect_rpc::RpcServer::new(Self::group(handler))
      }

      /// Mount `handler`'s `/rpc` routes on `router` (declared public routes; each procedure's
      /// own access decision is enforced per call).
      pub fn mount<S>(
        router: #p_::Router<#p_::DynEnv>,
        handler: S,
      ) -> #p_::Router<#p_::DynEnv>
      where
        S: #ident + ::core::marker::Send + ::core::marker::Sync + 'static,
      {
        Self::server(handler).mount(router)
      }

      /// A client calling this service through `transport`.
      pub fn client<T: #rt::RpcTransport>(transport: T) -> #client_ty<T> {
        #client_ty::new(transport)
      }
    }

    impl #rt::RpcService for #service_ty {
      const NAME: &'static str = #service_ty::NAME;
      const VERSION: u32 = #service_ty::VERSION;

      fn contract() -> ::id_effect_rpc::ServiceContract {
        #service_ty::contract()
      }
    }

    #[doc = #client_doc]
    #[derive(::core::clone::Clone, ::core::fmt::Debug)]
    #vis struct #client_ty<T> {
      transport: T,
    }

    impl<T: #rt::RpcTransport> #client_ty<T> {
      /// The service version this client calls.
      pub const VERSION: u32 = #version;

      /// A client over `transport`.
      pub fn new(transport: T) -> Self {
        Self { transport }
      }

      /// The transport.
      pub fn transport(&self) -> &T {
        &self.transport
      }

      #(#methods)*
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use quote::quote;

  fn ok(attr: TokenStream, item: TokenStream) -> String {
    expand(attr, item).expect("expands").to_string()
  }

  fn err(attr: TokenStream, item: TokenStream) -> String {
    let message = expand(attr, item).expect_err("refused").to_string();
    assert!(message.contains("\n  help: "), "no help line: {message}");
    message
  }

  fn v1() -> TokenStream {
    quote!(version = 1)
  }

  #[test]
  fn every_kind_expands_to_registration_contract_and_client() {
    let out = ok(
      quote!(version = 2, name = "Greet"),
      quote! {
        /// Says hello.
        pub trait Greeter {
          /// Greets.
          #[query]
          #[public]
          fn greet(&self, name: String) -> Effect<String, Never, DynEnv>;
          #[query(name = "now")]
          #[public]
          fn clock(&self) -> Effect<i64, Never, DynEnv>;
          #[mutation]
          #[guard(Staff)]
          #[guard(Admin::new())]
          fn shout(&self, text: String) -> Effect<String, Never, DynEnv>;
          #[server_stream]
          #[public]
          fn count(&self, n: i64) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>;
          #[client_stream(max_items = 10, max_bytes = 100)]
          #[public]
          fn sum(&self, items: Stream<i64, Never, DynEnv>) -> Effect<i64, Never, DynEnv>;
          #[bidi]
          #[public]
          fn echo(&self) -> Effect<Duplex<String, String, Never>, Never, DynEnv>;
          #[subscription]
          #[public]
          fn ticks(&self, from: i64, after: Option<Cursor>) -> Effect<Subscription<i64, Never>, Never, DynEnv>;
          #[subscription]
          #[public]
          fn news(&self, topic: String) -> Effect<Subscription<String, Never>, Never, DynEnv>;
          #[subscription()]
          #[public]
          fn all_news(&self) -> Effect<Subscription<String, Never>, Never, DynEnv>;
        }
      },
    );
    for needle in [
      "pub struct GreeterService",
      "pub struct GreeterClient",
      "\"Greet.greet\"",
      "\"Greet.now\"",
      ". guard (Staff) . guard (Admin :: new ())",
      ". max_items (10) . max_bytes (100)",
      ". describe (\"Greets.\")",
      ". with_description (\"Says hello.\")",
      "call_unary",
      "call_server_stream",
      "call_client_stream",
      "call_bidi",
      "call_subscribe",
      "handler . ticks (input , after)",
      "handler . news (input)",
      "handler . all_news ()",
      "handler . clock ()",
      "SerdeProbe",
      "handler . echo ()",
      "handler . sum (input)",
      "pub const VERSION : u32 = 2u32",
    ] {
      assert!(out.contains(needle), "missing `{needle}` in {out}");
    }
    assert!(
      !out.contains("# [query"),
      "procedure attributes are stripped"
    );
  }

  #[test]
  fn an_empty_service_still_has_a_contract() {
    let out = ok(
      v1(),
      quote!(
        trait Empty {}
      ),
    );
    assert!(out.contains("let group = :: id_effect_rpc :: RpcGroup :: new ()"));
    assert!(out.contains("\"Empty\""));
  }

  #[test]
  fn the_version_is_required_and_positive() {
    let t = quote!(
      trait A {}
    );
    assert!(err(quote!(), t.clone()).contains("needs a contract version"));
    assert!(err(quote!(name = "A"), t.clone()).contains("needs a contract version"));
    assert!(err(quote!(version = 0), t).contains("not a service version"));
  }

  #[test]
  fn service_arguments_are_typed_and_known() {
    let t = quote!(
      trait A {}
    );
    assert!(err(quote!(version = "1"), t.clone()).contains("is a number"));
    assert!(err(quote!(version = 1, name = 2), t.clone()).contains("is a string"));
    assert!(err(quote!(version = 1, name = "a.b"), t.clone()).contains("not a service name"));
    assert!(err(quote!(version = 1, version = 2), t.clone()).contains("unknown or repeated"));
    assert!(err(quote!(version = 1, other = 2), t.clone()).contains("unknown or repeated"));
    assert!(err(quote!(version), t).contains("cannot read"));
  }

  #[test]
  fn trait_shapes_are_checked() {
    assert!(
      err(
        v1(),
        quote!(
          struct A;
        )
      )
      .contains("goes on a trait")
    );
    assert!(
      err(
        v1(),
        quote!(
          trait A<T> {}
        )
      )
      .contains("cannot be generic")
    );
    assert!(
      err(
        v1(),
        quote!(
          trait A {
            const N: u8;
          }
        )
      )
      .contains("only procedures")
    );
  }

  #[test]
  fn the_service_name_is_checked() {
    let long = format_ident!("{}", "A".repeat(70));
    assert!(err(v1(), quote!(trait #long {})).contains("not a service name"));
  }

  fn method(attrs: TokenStream, sig: TokenStream) -> String {
    err(v1(), quote!(trait A { #attrs #sig; }))
  }

  #[test]
  fn kinds_and_access_decisions_are_required_once() {
    let greet = quote!(fn greet(&self, n: String) -> Effect<String, Never, DynEnv>);
    assert!(method(quote!(#[public]), greet.clone()).contains("no procedure kind"));
    assert!(method(quote!(#[query]), greet.clone()).contains("no access decision"));
    assert!(method(quote!(#[query] #[mutation] #[public]), greet).contains("two kinds"));
  }

  #[test]
  fn access_decisions_cannot_conflict() {
    let greet = quote!(fn greet(&self, n: String) -> Effect<String, Never, DynEnv>);
    assert!(method(quote!(#[query] #[public] #[public]), greet.clone()).contains("conflicting"));
    assert!(method(quote!(#[query] #[public] #[guard(G)]), greet.clone()).contains("conflicting"));
    assert!(method(quote!(#[query] #[guard(G)] #[public]), greet.clone()).contains("conflicting"));
    assert!(method(quote!(#[query] #[guard(,)]), greet.clone()).contains("as an expression"));
    assert!(expand(v1(), quote!(trait A { #[query] #[public(x)] #greet; })).is_err());
  }

  #[test]
  fn procedure_options_and_names_are_checked() {
    let greet = quote!(fn greet(&self, n: String) -> Effect<String, Never, DynEnv>);
    assert!(
      method(quote!(#[query(name = "a.b")] #[public]), greet.clone())
        .contains("not a procedure name")
    );
    assert!(
      method(quote!(#[query(retries = 3)] #[public]), greet).contains("unknown procedure option")
    );
    let twice = quote! {
      trait A {
        #[query] #[public] fn a(&self) -> Effect<i64, Never, DynEnv>;
        #[query(name = "a")] #[public] fn b(&self) -> Effect<i64, Never, DynEnv>;
      }
    };
    assert!(err(v1(), twice).contains("two procedures are named `a`"));
  }

  #[test]
  fn receivers_async_and_generics_are_checked() {
    let q = quote!(#[query] #[public]);
    let m = |sig: TokenStream| method(q.clone(), sig);
    assert!(m(quote!(async fn a(&self) -> Effect<i64, Never, DynEnv>)).contains("`async`"));
    assert!(m(quote!(fn a<T>(&self) -> Effect<T, Never, DynEnv>)).contains("generic"));
    assert!(m(quote!(fn a(&mut self) -> Effect<i64, Never, DynEnv>)).contains("`&self`"));
    assert!(m(quote!(fn a() -> Effect<i64, Never, DynEnv>)).contains("`&self`"));
  }

  #[test]
  fn parameters_and_returns_are_checked() {
    let q = quote!(#[query] #[public]);
    let m = |sig: TokenStream| method(q.clone(), sig);
    assert!(
      m(quote!(fn a(&self, (x, y): (i64, i64)) -> Effect<i64, Never, DynEnv>))
        .contains("plain names")
    );
    assert!(m(quote!(fn a(&self))).contains("does not return an `Effect`"));
    assert!(m(quote!(fn a(&self) -> i64)).contains("does not return an `Effect`"));
    assert!(
      m(quote!(fn a(&self, x: i64, y: i64) -> Effect<i64, Never, DynEnv>)).contains("too many")
    );
  }

  #[test]
  fn stream_shapes_are_checked() {
    let ss = quote!(#[server_stream] #[public]);
    assert!(
      method(ss, quote!(fn a(&self) -> Effect<i64, Never, DynEnv>))
        .contains("does not return a `Stream`")
    );
    let cs = quote!(#[client_stream] #[public]);
    assert!(
      method(
        cs.clone(),
        quote!(fn a(&self, x: i64) -> Effect<i64, Never, DynEnv>)
      )
      .contains("does not take a `Stream`")
    );
    assert!(
      method(cs, quote!(fn a(&self) -> Effect<i64, Never, DynEnv>))
        .contains("does not take a `Stream`")
    );
  }

  #[test]
  fn bidi_and_subscription_shapes_are_checked() {
    let bidi = quote!(#[bidi] #[public]);
    assert!(
      method(
        bidi.clone(),
        quote!(fn a(&self) -> Effect<Stream<i64, Never, DynEnv>, Never, DynEnv>)
      )
      .contains("does not return a `Duplex`")
    );
    assert!(
      method(
        bidi,
        quote!(fn a(&self, x: i64) -> Effect<Duplex<i64, i64>, Never, DynEnv>)
      )
      .contains("too many")
    );
    let sub = quote!(#[subscription] #[public]);
    assert!(
      method(
        sub.clone(),
        quote!(fn a(&self, x: i64) -> Effect<i64, Never, DynEnv>)
      )
      .contains("does not return a `Subscription`")
    );
    assert!(
      method(
        sub,
        quote!(fn a(&self, x: i64, c: Option<Cursor>, z: i64) -> Effect<Subscription<i64>, Never, DynEnv>)
      )
      .contains("too many")
    );
  }

  #[test]
  fn kind_names_round_trip() {
    for (name, kind) in KINDS {
      assert_eq!(kind.attr(), name);
      assert_eq!(kind.ident(), name);
      assert!(kind.contract_kind().to_string().contains("ProcedureKind"));
    }
  }
}
