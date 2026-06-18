//! [`Lens`] — total focus into a subpart of a product type.

use std::sync::Arc;

/// Total optic: every `S` has exactly one focused `A`.
#[derive(Clone)]
pub struct Lens<S, A> {
  get: Arc<dyn Fn(&S) -> A + Send + Sync>,
  set: Arc<dyn Fn(S, A) -> S + Send + Sync>,
}

impl<S: 'static, A: 'static> Lens<S, A> {
  /// Build a lens from getter and setter.
  pub fn new(
    get: impl Fn(&S) -> A + Send + Sync + 'static,
    set: impl Fn(S, A) -> S + Send + Sync + 'static,
  ) -> Self
  where
    A: Clone,
  {
    Self {
      get: Arc::new(get),
      set: Arc::new(set),
    }
  }

  /// Read the focused value.
  pub fn get(&self, source: &S) -> A
  where
    A: Clone,
  {
    (self.get)(source)
  }

  /// Replace the focused value, returning an updated `S`.
  pub fn set(&self, source: S, value: A) -> S {
    (self.set)(source, value)
  }

  /// Transform the focused value with `f`.
  pub fn modify(&self, source: S, f: impl FnOnce(A) -> A) -> S
  where
    A: Clone,
  {
    let value = self.get(&source);
    self.set(source, f(value))
  }

  /// Compose with an inner lens: focus `S → A → B`.
  pub fn compose<B>(self, inner: Lens<A, B>) -> Lens<S, B>
  where
    A: Clone + 'static,
    B: Clone + 'static,
  {
    let outer_get = self.get.clone();
    let outer_set = self.set.clone();
    let inner_get = inner.get.clone();
    let inner_set = inner.set.clone();
    let outer_get_read = outer_get.clone();
    Lens::new(
      move |s| inner_get(&outer_get_read(s)),
      move |s, b| {
        let a = outer_get(&s);
        let a = inner_set(a, b);
        outer_set(s, a)
      },
    )
  }
}

/// Lens that always reads/writes the whole value (`S = A`).
pub fn identity_lens<S: Clone + 'static>() -> Lens<S, S> {
  Lens::new(|s: &S| s.clone(), |_, a| a)
}

/// Field lens for structs with cloneable fields.
pub fn field<S, A, FGet, FSet>(get: FGet, set: FSet) -> Lens<S, A>
where
  S: 'static,
  A: Clone + 'static,
  FGet: Fn(&S) -> &A + Send + Sync + 'static,
  FSet: Fn(S, A) -> S + Send + Sync + 'static,
{
  Lens::new(move |s| get(s).clone(), set)
}

#[cfg(test)]
mod tests {
  use super::*;
  use rstest::rstest;

  #[derive(Clone, Debug, PartialEq)]
  struct Address {
    city: String,
  }

  #[derive(Clone, Debug, PartialEq)]
  struct Person {
    name: String,
    address: Address,
  }

  fn person_lens() -> Lens<Person, String> {
    field(
      |p: &Person| &p.name,
      |mut p, name| {
        p.name = name;
        p
      },
    )
  }

  fn address_lens() -> Lens<Person, Address> {
    field(
      |p: &Person| &p.address,
      |mut p, address| {
        p.address = address;
        p
      },
    )
  }

  fn city_lens() -> Lens<Address, String> {
    field(
      |a: &Address| &a.city,
      |mut a, city| {
        a.city = city;
        a
      },
    )
  }

  mod get {
    use super::*;

    #[test]
    fn returns_focused_field() {
      let person = Person {
        name: "Ada".into(),
        address: Address {
          city: "London".into(),
        },
      };
      assert_eq!(person_lens().get(&person), "Ada");
    }
  }

  mod set {
    use super::*;

    #[test]
    fn replaces_focused_field() {
      let person = Person {
        name: "Ada".into(),
        address: Address {
          city: "London".into(),
        },
      };
      let updated = person_lens().set(person, "Grace".into());
      assert_eq!(updated.name, "Grace");
      assert_eq!(updated.address.city, "London");
    }
  }

  mod modify {
    use super::*;

    #[test]
    fn transforms_focused_field() {
      let person = Person {
        name: "ada".into(),
        address: Address {
          city: "London".into(),
        },
      };
      let updated = person_lens().modify(person, |name| name.to_uppercase());
      assert_eq!(updated.name, "ADA");
    }
  }

  mod compose {
    use super::*;

    #[test]
    fn chains_nested_field_access() {
      let person = Person {
        name: "Ada".into(),
        address: Address {
          city: "London".into(),
        },
      };
      let city = address_lens().compose(city_lens());
      assert_eq!(city.get(&person), "London");
      let updated = city.set(person, "Paris".into());
      assert_eq!(updated.address.city, "Paris");
    }
  }

  mod identity_lens {
    use super::*;

    #[rstest]
    #[case::get_roundtrip("value".to_string())]
    fn get_and_set_roundtrip(#[case] value: String) {
      let lens = identity_lens::<String>();
      assert_eq!(lens.get(&value), value);
      assert_eq!(lens.set(value.clone(), "next".into()), "next");
    }
  }
}
