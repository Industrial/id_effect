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

  /// View this lens as a traversal over exactly one focus.
  pub fn as_traversal(&self) -> crate::traversal::Traversal<S, A>
  where
    S: Clone,
    A: Clone,
  {
    let read = self.clone();
    let write = self.clone();
    crate::traversal::Traversal::new(
      move |s, f| write.modify(s, f),
      move |s, visit| visit(read.get(s)),
    )
  }

  /// Compose with a traversal on the focused value.
  pub fn compose_traversal<B>(
    self,
    inner: crate::traversal::Traversal<A, B>,
  ) -> crate::traversal::Traversal<S, B>
  where
    S: Clone + 'static,
    A: Clone + 'static,
    B: Clone + 'static,
  {
    let read_for_modify = self.clone();
    let read_for_fold = self.clone();
    let write = self;
    let inner_for_modify = inner.clone();
    let inner_for_fold = inner;
    crate::traversal::Traversal::new(
      move |s, f| {
        let focused = read_for_modify.get(&s);
        let updated = inner_for_modify.over(focused, f);
        write.set(s, updated)
      },
      move |s, visit| {
        inner_for_fold.fold_each(&read_for_fold.get(&s), |item| visit(item));
      },
    )
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

  mod as_traversal {
    use super::*;

    #[test]
    fn maps_single_focus() {
      let person = Person {
        name: "Ada".into(),
        address: Address {
          city: "London".into(),
        },
      };
      let updated = person_lens()
        .as_traversal()
        .over(person, |n| n.to_uppercase());
      assert_eq!(updated.name, "ADA");
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
