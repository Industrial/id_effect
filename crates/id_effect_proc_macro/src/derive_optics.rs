//! `#[derive(Optics)]` — generates field lenses and variant prisms for `id_effect_optics`.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, Type, parse_macro_input};

/// Expand optics helpers for structs (field lenses) and enums (variant prisms).
pub fn derive_optics(input: TokenStream) -> TokenStream {
  let input = parse_macro_input!(input as DeriveInput);
  let ident = &input.ident;
  let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

  let items = match &input.data {
    Data::Struct(data) => derive_struct_items(data),
    Data::Enum(data) => derive_enum_items(data),
    Data::Union(un) => {
      return syn::Error::new_spanned(un.union_token, "Optics derive does not support unions")
        .to_compile_error()
        .into();
    }
  };

  quote! {
    impl #impl_generics #ident #ty_generics #where_clause {
      #(#items)*
    }
  }
  .into()
}

fn derive_struct_items(data: &syn::DataStruct) -> Vec<proc_macro2::TokenStream> {
  let Fields::Named(fields) = &data.fields else {
    return Vec::new();
  };

  fields
    .named
    .iter()
    .filter(|field| !has_optics_skip(&field.attrs))
    .flat_map(|field| {
      let field_ident = field.ident.as_ref().expect("named field");
      let lens_fn = format_ident!("{}_lens", field_ident);
      let field_type = &field.ty;

      let lens = quote! {
        #[doc(hidden)]
        pub fn #lens_fn() -> ::id_effect_optics::Lens<Self, #field_type> {
          ::id_effect_optics::field(
            |s: &Self| &s.#field_ident,
            |mut s, value| {
              s.#field_ident = value;
              s
            },
          )
        }
      };

      if is_option_type(field_type) {
        let optional_fn = format_ident!("{}_optional", field_ident);
        let inner = option_inner_type(field_type);
        vec![
          lens,
          quote! {
            #[doc(hidden)]
            pub fn #optional_fn() -> ::id_effect_optics::Optional<Self, #inner> {
              ::id_effect_optics::Optional::new(Self::#lens_fn())
            }
          },
        ]
      } else {
        vec![lens]
      }
    })
    .collect()
}

fn derive_enum_items(data: &syn::DataEnum) -> Vec<proc_macro2::TokenStream> {
  data
    .variants
    .iter()
    .filter(|variant| !has_optics_skip(&variant.attrs))
    .map(|variant| {
      let variant_ident = &variant.ident;
      let prism_fn = format_ident!("{}_prism", to_snake_case(&variant_ident.to_string()));
      let (preview, review, focus_ty) = variant_prism_parts(variant_ident, variant);

      quote! {
        #[doc(hidden)]
        pub fn #prism_fn() -> ::id_effect_optics::Prism<Self, #focus_ty> {
          ::id_effect_optics::Prism::new(#preview, #review)
        }
      }
    })
    .collect()
}

fn variant_prism_parts(
  variant_ident: &syn::Ident,
  variant: &syn::Variant,
) -> (
  proc_macro2::TokenStream,
  proc_macro2::TokenStream,
  proc_macro2::TokenStream,
) {
  match &variant.fields {
    Fields::Unit => (
      quote! { |s: &Self| match s { Self::#variant_ident => Some(()), _ => None } },
      quote! { |_| Self::#variant_ident },
      quote! { () },
    ),
    Fields::Unnamed(fields) if fields.unnamed.len() == 1 => {
      let inner = &fields.unnamed[0].ty;
      (
        quote! { |s: &Self| match s { Self::#variant_ident(value) => Some(value.clone()), _ => None } },
        quote! { |value| Self::#variant_ident(value) },
        quote! { #inner },
      )
    }
    Fields::Unnamed(fields) => {
      let tys: Vec<_> = fields.unnamed.iter().map(|f| &f.ty).collect();
      let bindings: Vec<_> = (0..tys.len())
        .map(|idx| format_ident!("v{}", idx))
        .collect();
      (
        quote! {
          |s: &Self| match s {
            Self::#variant_ident(#(#bindings),*) => Some((#(#bindings.clone(),)*)),
            _ => None,
          }
        },
        quote! { |(#(#bindings,)*)| Self::#variant_ident(#(#bindings),*) },
        quote! { (#(#tys,)*) },
      )
    }
    Fields::Named(fields) => {
      let bindings: Vec<_> = fields
        .named
        .iter()
        .map(|field| field.ident.as_ref().expect("named field"))
        .collect();
      let tys: Vec<_> = fields.named.iter().map(|field| &field.ty).collect();
      (
        quote! {
          |s: &Self| match s {
            Self::#variant_ident { #(#bindings),* } => Some((#(#bindings.clone(),)*)),
            _ => None,
          }
        },
        quote! { |(#(#bindings,)*)| Self::#variant_ident { #(#bindings),* } },
        quote! { (#(#tys,)*) },
      )
    }
  }
}

fn has_optics_skip(attrs: &[syn::Attribute]) -> bool {
  attrs.iter().any(|attr| {
    attr.path().is_ident("optics")
      && attr
        .parse_args::<syn::Ident>()
        .is_ok_and(|ident| ident == "skip")
  })
}

fn is_option_type(ty: &Type) -> bool {
  matches!(ty, Type::Path(path) if path.path.segments.last().is_some_and(|seg| seg.ident == "Option"))
}

fn option_inner_type(ty: &Type) -> proc_macro2::TokenStream {
  let Type::Path(path) = ty else {
    return quote! { _ };
  };
  let Some(last) = path.path.segments.last() else {
    return quote! { _ };
  };
  if last.ident == "Option"
    && let syn::PathArguments::AngleBracketed(args) = &last.arguments
    && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
  {
    return quote! { #inner };
  }
  quote! { _ }
}

fn to_snake_case(name: &str) -> String {
  let mut out = String::new();
  for (idx, ch) in name.chars().enumerate() {
    if ch.is_uppercase() {
      if idx > 0 {
        out.push('_');
      }
      out.push(ch.to_ascii_lowercase());
    } else {
      out.push(ch);
    }
  }
  out
}
