//! Fixtures and factories (feature `fixtures`): rows a test needs, with arbitrary but
//! **deterministic** defaults drawn from the type's schema, the fields the test cares about set
//! explicitly, inserted through the `SqlClientService` and read back as `FromRow` rows.
//!
//! - A [`Fixture`] is the value a test inserts (`NewAccount`): [`Fixture::arbitrary`] is its
//!   default-value strategy — built from its fields' [`Schema`](id_effect::Schema)s with
//!   [`from_schema`] / [`arb_schema`] / [`unknown_object`], so every default passes the same
//!   refinements the application checks — and [`Fixture::insert_sql`] the statement that inserts
//!   it and returns the row (`INSERT … RETURNING …`), decoded as [`Fixture::Row`]. Override
//!   [`Fixture::insert`] to insert some other way (a repository, several statements).
//! - A [`Factory`] draws fixtures from a seeded generator: the same seed gives the same values on
//!   every run and machine. Overrides ([`Factory::with`], [`Factory::with_seq`] for unique keys,
//!   [`Factory::build_with`] for one value) set what the test is about.
//!
//! ```rust
//! use id_effect::schema::{i64, refine, string};
//! use id_effect_sql::{SqlValue, params};
//! use id_effect_test::fixtures::{BoxedStrategy, Factory, Fixture, Strategy, from_schema};
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct NewAccount {
//!   name: String,
//!   balance: i64,
//! }
//!
//! impl Fixture for NewAccount {
//!   type Row = (i64, String, i64);
//!
//!   fn arbitrary() -> BoxedStrategy<Self> {
//!     let name = from_schema(refine(string::<()>(), |s: &String| !s.is_empty(), "non-empty"), "[a-z]{1,12}");
//!     let balance = from_schema(refine(i64::<()>(), |n| *n >= 0, "non-negative"), 0..1_000_000i64);
//!     (name, balance).prop_map(|(name, balance)| NewAccount { name, balance }).boxed()
//!   }
//!
//!   fn insert_sql(&self) -> (String, Vec<SqlValue>) {
//!     let sql = "INSERT INTO accounts (name, balance) VALUES ($1, $2) RETURNING id, name, balance";
//!     (sql.into(), params![self.name.clone(), self.balance])
//!   }
//! }
//!
//! let mut accounts = Factory::<NewAccount>::seeded(7).with_seq(|a, n| a.name = format!("acct-{n}"));
//! let first = accounts.build();
//! assert_eq!(first.name, "acct-1");
//! assert!(first.balance >= 0);
//! // Same seed, same values.
//! let again = Factory::<NewAccount>::seeded(7).with_seq(|a, n| a.name = format!("acct-{n}")).build();
//! assert_eq!(first, again);
//! let empty = accounts.build_with(|a| a.balance = 0);
//! assert_eq!((empty.name.as_str(), empty.balance), ("acct-2", 0));
//! ```

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, HList, Selector, succeed};
use id_effect_sql::{FromRow, SqlClientService, SqlError, SqlValue, query_one_as};
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

pub use id_effect::testing::property::{arb_schema, arb_unknown, from_schema, unknown_object};
pub use proptest;
pub use proptest::strategy::{BoxedStrategy, Just, Strategy};

/// The seed of [`Factory::new`].
pub const DEFAULT_SEED: u64 = 0x1d_effec7;

/// A value a test inserts: arbitrary schema-valid defaults, and how to insert it.
pub trait Fixture: Clone + fmt::Debug + Send + 'static {
  /// The inserted row, decoded from what [`insert_sql`](Self::insert_sql) returns.
  type Row: FromRow + Send + 'static;

  /// The defaults: a strategy over valid values, built from the fields' schemas
  /// ([`from_schema`], [`arb_schema`], [`unknown_object`]). A [`Factory`] draws from it with
  /// its seeded generator.
  fn arbitrary() -> BoxedStrategy<Self>;

  /// The statement inserting `self` and returning the row (`INSERT … RETURNING …`), with its
  /// parameters.
  fn insert_sql(&self) -> (String, Vec<SqlValue>);

  /// Inserts `self` through the `SqlClientService` in `R` and decodes the row
  /// ([`query_one_as`] over [`insert_sql`](Self::insert_sql)). Override it to insert another
  /// way.
  fn insert<R, I>(&self) -> Effect<Self::Row, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    let (sql, params) = self.insert_sql();
    query_one_as(sql, params)
  }
}

/// An override a factory applies to every value it builds: the value and its sequence number.
type Override<T> = Arc<dyn Fn(&mut T, u64) + Send + Sync>;

/// Builds [`Fixture`]s from a seeded generator, applies the overrides, and inserts them.
///
/// Values are drawn from [`Fixture::arbitrary`] with a ChaCha generator seeded from the
/// factory's seed, so a factory built with the same seed and overrides yields the same sequence
/// of values everywhere. The sequence number ([`Factory::seq`]) counts the values built, from 1.
pub struct Factory<T: Fixture> {
  seed: u64,
  runner: TestRunner,
  seq: u64,
  overrides: Vec<Override<T>>,
}

impl<T: Fixture> Factory<T> {
  /// A factory seeded with [`DEFAULT_SEED`].
  pub fn new() -> Self {
    Self::seeded(DEFAULT_SEED)
  }

  /// A factory whose values are determined by `seed`.
  pub fn seeded(seed: u64) -> Self {
    let mut bytes = [0u8; 32];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
      let word = seed.rotate_left(16 * i as u32) ^ (i as u64);
      chunk.copy_from_slice(&word.to_le_bytes());
    }
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &bytes);
    Self {
      seed,
      runner: TestRunner::new_with_rng(Config::default(), rng),
      seq: 0,
      overrides: Vec::new(),
    }
  }

  /// The seed.
  pub fn seed(&self) -> u64 {
    self.seed
  }

  /// The number of values built so far (the last value's sequence number).
  pub fn seq(&self) -> u64 {
    self.seq
  }

  /// Applies `f` to every value built from now on (after the earlier overrides).
  #[must_use]
  pub fn with(self, f: impl Fn(&mut T) + Send + Sync + 'static) -> Self {
    self.with_seq(move |value, _| f(value))
  }

  /// Applies `f` to every value built from now on, with its sequence number (1, 2, …): unique
  /// keys such as `format!("user-{n}@example.com")`.
  #[must_use]
  pub fn with_seq(mut self, f: impl Fn(&mut T, u64) + Send + Sync + 'static) -> Self {
    self.overrides.push(Arc::new(f));
    self
  }

  /// The next value: drawn from [`Fixture::arbitrary`], then the overrides.
  ///
  /// # Panics
  ///
  /// When the strategy cannot produce a value (its filters reject every candidate proptest
  /// tries): give the schema a wire strategy that mostly decodes.
  #[track_caller]
  pub fn build(&mut self) -> T {
    self.build_with(|_| {})
  }

  /// The next value with `f` applied last (this value only).
  #[track_caller]
  pub fn build_with(&mut self, f: impl FnOnce(&mut T)) -> T {
    let mut value = match T::arbitrary().new_tree(&mut self.runner) {
      Ok(tree) => tree.current(),
      Err(reason) => panic!(
        "Factory<{}>: the arbitrary strategy produced no value: {reason}",
        std::any::type_name::<T>()
      ),
    };
    self.seq += 1;
    for o in &self.overrides {
      o(&mut value, self.seq);
    }
    f(&mut value);
    value
  }

  /// The next `n` values.
  #[track_caller]
  pub fn build_many(&mut self, n: usize) -> Vec<T> {
    (0..n).map(|_| self.build()).collect()
  }

  /// Builds the next value now and inserts it when the effect runs ([`Fixture::insert`]).
  pub fn insert<R, I>(&mut self) -> Effect<T::Row, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    self.build().insert()
  }

  /// [`insert`](Self::insert) with `f` applied to the value.
  pub fn insert_with<R, I>(&mut self, f: impl FnOnce(&mut T)) -> Effect<T::Row, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    self.build_with(f).insert()
  }

  /// Builds the next `n` values now and inserts them in order when the effect runs, stopping at
  /// the first failure.
  pub fn insert_many<R, I>(&mut self, n: usize) -> Effect<Vec<T::Row>, SqlError, R>
  where
    R: Selector<SqlClientService, I> + HList,
  {
    self
      .build_many(n)
      .into_iter()
      .fold(succeed(Vec::with_capacity(n)), |rows, value| {
        rows.flat_map(move |mut rows| {
          value.insert().map(move |row| {
            rows.push(row);
            rows
          })
        })
      })
  }
}

impl<T: Fixture> Default for Factory<T> {
  fn default() -> Self {
    Self::new()
  }
}

impl<T: Fixture> fmt::Debug for Factory<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Factory")
      .field("fixture", &std::any::type_name::<T>())
      .field("seed", &self.seed)
      .field("seq", &self.seq)
      .field("overrides", &self.overrides.len())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema::{i64, refine};

  #[derive(Clone, Debug, PartialEq)]
  struct Even(i64);

  impl Fixture for Even {
    type Row = (i64,);

    fn arbitrary() -> BoxedStrategy<Self> {
      from_schema(refine(i64::<()>(), |n| n % 2 == 0, "even"), -1000..1000i64)
        .prop_map(Even)
        .boxed()
    }

    fn insert_sql(&self) -> (String, Vec<SqlValue>) {
      (
        "INSERT INTO t VALUES ($1) RETURNING n".into(),
        vec![self.0.into()],
      )
    }
  }

  #[derive(Clone, Debug)]
  struct Never;

  impl Fixture for Never {
    type Row = (i64,);

    fn arbitrary() -> BoxedStrategy<Self> {
      Just(Never).prop_filter("nothing passes", |_| false).boxed()
    }

    fn insert_sql(&self) -> (String, Vec<SqlValue>) {
      (String::new(), Vec::new())
    }
  }

  #[test]
  fn a_seed_determines_the_values() {
    let a = Factory::<Even>::seeded(1).build_many(20);
    assert_eq!(a, Factory::<Even>::seeded(1).build_many(20));
    assert_ne!(a, Factory::<Even>::seeded(2).build_many(20));
    assert!(a.iter().all(|Even(n)| n % 2 == 0), "{a:?}");
    assert_eq!(Factory::<Even>::default().seed(), DEFAULT_SEED);
    assert_eq!(Even(2).insert_sql().1, vec![SqlValue::I64(2)]);
    assert!(Never.insert_sql().0.is_empty());
  }

  #[test]
  fn overrides_apply_in_order_with_the_sequence_number() {
    let mut f = Factory::<Even>::new()
      .with(|e| e.0 = 10)
      .with_seq(|e, n| e.0 += i64::try_from(n).unwrap());
    assert_eq!(f.build(), Even(11));
    assert_eq!(f.build_with(|e| e.0 *= 2), Even(24));
    assert_eq!(f.seq(), 2);
    let shown = format!("{f:?}");
    assert!(
      shown.contains("seq: 2") && shown.contains("overrides: 2"),
      "{shown}"
    );
  }

  #[test]
  #[should_panic(expected = "produced no value")]
  fn a_strategy_without_values_panics_with_the_fixture() {
    let _ = Factory::<Never>::new().build();
  }
}
