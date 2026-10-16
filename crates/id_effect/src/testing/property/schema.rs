//! Generators derived from a [`Schema`] / [`HasSchema`], JSON-shaped [`Unknown`] wires, and the
//! codec round-trip property.
//!
//! A schema is a decoder, so the generators draw *wire* values and keep the ones the schema
//! decodes: every generated value passes the schema's refinements, brands and filters, and a
//! failing case shrinks on the wire value.

use std::collections::BTreeMap;
use std::fmt::Debug;

use proptest::arbitrary::Arbitrary;
use proptest::collection::{btree_map, vec};
use proptest::prelude::{BoxedStrategy, Just, Strategy, any};
use proptest::prop_oneof;
use proptest::test_runner::TestCaseError;

use crate::schema::{HasSchema, Schema, Unknown};

/// The values `schema` decodes from wire values drawn from `wire`.
///
/// Wire values the schema rejects are discarded (proptest counts them as local rejects and gives
/// up after too many), so pick a `wire` strategy that mostly decodes: e.g. `0..150i64` for an
/// "age" refinement rather than `any::<i64>()`.
///
/// ```
/// use id_effect::schema::{i64, refine};
/// use id_effect::testing::property::from_schema;
/// use proptest::prelude::*;
///
/// let even = refine(i64::<()>(), |n| n % 2 == 0, "even");
/// proptest!(|(n in from_schema(even, -1000i64..1000))| {
///   prop_assert_eq!(n % 2, 0);
/// });
/// ```
pub fn from_schema<A, I, E, S>(schema: Schema<A, I, E>, wire: S) -> impl Strategy<Value = A>
where
  A: Debug,
  S: Strategy<Value = I>,
{
  wire.prop_filter_map("rejected by the schema", move |i| schema.decode(i).ok())
}

/// The values of `T`'s [`HasSchema`] schema, decoded from arbitrary wire values
/// (`T::I: Arbitrary`), e.g. a newtype or brand over a primitive wire.
///
/// For a JSON-wire schema (`#[derive(Schema)]`, `I = Unknown`) pass a wire to [`from_schema`]
/// instead, built with [`unknown_object`]: arbitrary `Unknown` trees almost never decode.
pub fn arb_schema<T>() -> impl Strategy<Value = T::A>
where
  T: HasSchema,
  T::A: Debug,
  T::I: Arbitrary,
{
  from_schema(T::schema(), any::<T::I>())
}

/// Arbitrary JSON-shaped [`Unknown`] trees (finite numbers; arrays and objects up to depth 3).
pub fn arb_unknown() -> impl Strategy<Value = Unknown> {
  let leaf = prop_oneof![
    Just(Unknown::Null),
    any::<bool>().prop_map(Unknown::Bool),
    any::<i64>().prop_map(Unknown::I64),
    (-1.0e9..1.0e9f64).prop_map(Unknown::F64),
    "[ -~]{0,12}".prop_map(Unknown::String),
  ];
  leaf.prop_recursive(3, 32, 4, |inner| {
    prop_oneof![
      vec(inner.clone(), 0..4).prop_map(Unknown::Array),
      btree_map("[a-z]{1,6}", inner, 0..4).prop_map(Unknown::Object),
    ]
  })
}

/// An [`Unknown::Object`] with exactly the given fields, each drawn from its strategy: the wire
/// for a `#[derive(Schema)]` struct, field by field (use the wire names).
///
/// ```
/// use id_effect::schema::Unknown;
/// use id_effect::testing::property::unknown_object;
/// use proptest::prelude::*;
///
/// let wire = unknown_object([
///   ("user_name", "[a-z]{1,8}".prop_map(Unknown::String).boxed()),
///   ("age", (0..150i64).prop_map(Unknown::I64).boxed()),
/// ]);
/// proptest!(|(u in wire)| {
///   let Unknown::Object(fields) = u else { unreachable!() };
///   prop_assert_eq!(fields.len(), 2);
/// });
/// ```
pub fn unknown_object(
  fields: impl IntoIterator<Item = (&'static str, BoxedStrategy<Unknown>)>,
) -> impl Strategy<Value = Unknown> {
  let (names, strategies): (Vec<&'static str>, Vec<BoxedStrategy<Unknown>>) =
    fields.into_iter().unzip();
  strategies.prop_map(move |values| {
    Unknown::Object(
      names
        .iter()
        .map(|n| (*n).to_owned())
        .zip(values)
        .collect::<BTreeMap<_, _>>(),
    )
  })
}

/// `Ok` if `value` survives `schema.encode` then `schema.decode` unchanged.
pub fn prop_assert_roundtrip<A, I, E>(
  schema: &Schema<A, I, E>,
  value: A,
) -> Result<(), TestCaseError>
where
  A: Clone + Debug + PartialEq,
{
  match schema.decode(schema.encode(value.clone())) {
    Ok(back) if back == value => Ok(()),
    Ok(back) => Err(TestCaseError::fail(format!(
      "round trip changed the value: {value:?} came back as {back:?}"
    ))),
    Err(e) => Err(TestCaseError::fail(format!(
      "round trip failed to decode {value:?}: {e:?}"
    ))),
  }
}

#[cfg(test)]
mod tests {
  use proptest::prelude::*;

  use super::*;
  use crate::schema::{ParseError, filter, i64, string};

  struct Port;

  impl HasSchema for Port {
    type A = u16;
    type I = i64;
    type E = ();

    fn schema() -> Schema<u16, i64, ()> {
      Schema::make(
        |n: i64| u16::try_from(n).map_err(|_| ParseError::new("", "port out of range")),
        i64::from,
        |_u: &Unknown| Err(ParseError::new("", "unused")),
      )
    }
  }

  fn short_name() -> Schema<String, String, ()> {
    filter(string::<()>(), |s: &String| s.len() <= 4, "too long")
  }

  proptest! {
    #[test]
    fn from_schema_yields_only_accepted_values(s in from_schema(short_name(), "[a-z]{0,6}")) {
      prop_assert!(s.len() <= 4);
    }

    #[test]
    fn has_schema_values_roundtrip(port in from_schema(Port::schema(), 0..70_000i64)) {
      prop_assert_roundtrip(&Port::schema(), port)?;
    }

    #[test]
    fn i64_schema_roundtrips(n in arb_schema::<I64Wire>()) {
      prop_assert_roundtrip(&i64::<()>(), n)?;
    }

    #[test]
    fn unknown_trees_have_finite_numbers(u in arb_unknown()) {
      fn finite(u: &Unknown) -> bool {
        match u {
          Unknown::F64(f) => f.is_finite(),
          Unknown::Array(xs) => xs.iter().all(finite),
          Unknown::Object(m) => m.values().all(finite),
          _ => true,
        }
      }
      prop_assert!(finite(&u));
    }

    #[test]
    fn unknown_object_has_exactly_the_fields(u in unknown_object([
      ("a", any::<bool>().prop_map(Unknown::Bool).boxed()),
      ("b", Just(Unknown::Null).boxed()),
    ])) {
      let Unknown::Object(m) = u else { panic!("not an object") };
      prop_assert_eq!(m.keys().map(String::as_str).collect::<Vec<_>>(), vec!["a", "b"]);
    }
  }

  struct I64Wire;

  impl HasSchema for I64Wire {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<i64, i64, ()> {
      i64::<()>()
    }
  }

  #[test]
  fn roundtrip_reports_a_lossy_schema() {
    let lossy: Schema<i64, i64, ()> = Schema::make(|n: i64| Ok(n / 2), |n| n, |_u: &Unknown| Ok(0));
    let err = prop_assert_roundtrip(&lossy, 8).unwrap_err();
    assert!(err.to_string().contains("came back as 4"), "{err}");
    let rejecting: Schema<i64, i64, ()> = Schema::make(
      |_n: i64| Err(ParseError::new("", "never")),
      |n| n,
      |_u: &Unknown| Ok(0),
    );
    assert!(prop_assert_roundtrip(&rejecting, 1).is_err());
  }
}
