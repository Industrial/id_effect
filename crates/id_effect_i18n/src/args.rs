//! [`Args`]: the named arguments of a message (`{ $count }`, `{ $name }`).

use std::borrow::Cow;

use fluent_bundle::{FluentArgs, FluentValue};

/// One argument value: text or a number.
///
/// Numbers drive Fluent selectors such as `{ $count -> [one] ... *[other] ... }`, which pick the
/// CLDR plural category of the message's locale.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
  /// A string, inserted as is (Fluent does not escape; HTML escaping is the renderer's job).
  Text(String),
  /// An integer.
  Int(i64),
  /// A floating-point number.
  Float(f64),
}

impl From<&str> for ArgValue {
  fn from(v: &str) -> Self {
    Self::Text(v.to_owned())
  }
}

impl From<String> for ArgValue {
  fn from(v: String) -> Self {
    Self::Text(v)
  }
}

impl From<f64> for ArgValue {
  fn from(v: f64) -> Self {
    Self::Float(v)
  }
}

impl From<f32> for ArgValue {
  fn from(v: f32) -> Self {
    Self::Float(f64::from(v))
  }
}

macro_rules! int_arg {
  ($($t:ty),*) => {
    $(impl From<$t> for ArgValue {
      fn from(v: $t) -> Self {
        Self::Int(i64::from(v))
      }
    })*
  };
}

int_arg!(i8, i16, i32, i64, u8, u16, u32);

macro_rules! wide_arg {
  ($($t:ty),*) => {
    $(impl From<$t> for ArgValue {
      /// An integer; a value above `i64::MAX` becomes the nearest `Float`.
      fn from(v: $t) -> Self {
        i64::try_from(v).map_or(Self::Float(v as f64), Self::Int)
      }
    })*
  };
}

wide_arg!(u64, usize);

/// The named arguments of one message, in insertion order (a later value for the same name wins).
///
/// ```
/// use id_effect_i18n::{ArgValue, Args};
///
/// let args = Args::new().with("name", "Ada").with("count", 3);
/// assert_eq!(args.get("count"), Some(&ArgValue::Int(3)));
/// assert_eq!(args.len(), 2);
/// ```
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Args {
  values: Vec<(Cow<'static, str>, ArgValue)>,
}

impl Args {
  /// No arguments.
  pub fn new() -> Self {
    Self::default()
  }

  /// These arguments with `name` set to `value`.
  #[must_use]
  pub fn with(mut self, name: impl Into<Cow<'static, str>>, value: impl Into<ArgValue>) -> Self {
    self.set(name, value);
    self
  }

  /// Sets `name` to `value`, replacing an earlier value of the same name.
  pub fn set(&mut self, name: impl Into<Cow<'static, str>>, value: impl Into<ArgValue>) {
    let name = name.into();
    let value = value.into();
    match self.values.iter_mut().find(|(n, _)| *n == name) {
      Some(slot) => slot.1 = value,
      None => self.values.push((name, value)),
    }
  }

  /// The value of `name`, if set.
  pub fn get(&self, name: &str) -> Option<&ArgValue> {
    self
      .values
      .iter()
      .find_map(|(n, v)| (n == name).then_some(v))
  }

  /// The number of arguments.
  pub fn len(&self) -> usize {
    self.values.len()
  }

  /// Whether there are no arguments.
  pub fn is_empty(&self) -> bool {
    self.values.is_empty()
  }

  /// The arguments as Fluent sees them.
  pub(crate) fn to_fluent(&self) -> FluentArgs<'_> {
    let mut out = FluentArgs::with_capacity(self.values.len());
    for (name, value) in &self.values {
      let value = match value {
        ArgValue::Text(s) => FluentValue::from(s.as_str()),
        ArgValue::Int(n) => FluentValue::from(*n),
        ArgValue::Float(n) => FluentValue::from(*n),
      };
      out.set(name.as_ref(), value);
    }
    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn arg_values_convert_from_strings_and_numbers() {
    assert_eq!(ArgValue::from("a"), ArgValue::Text("a".into()));
    assert_eq!(
      ArgValue::from(String::from("b")),
      ArgValue::Text("b".into())
    );
    assert_eq!(ArgValue::from(1.5_f64), ArgValue::Float(1.5));
    assert_eq!(ArgValue::from(0.5_f32), ArgValue::Float(0.5));
    assert_eq!(ArgValue::from(-3_i8), ArgValue::Int(-3));
    assert_eq!(ArgValue::from(-3_i16), ArgValue::Int(-3));
    assert_eq!(ArgValue::from(-3_i32), ArgValue::Int(-3));
    assert_eq!(ArgValue::from(-3_i64), ArgValue::Int(-3));
    assert_eq!(ArgValue::from(3_u8), ArgValue::Int(3));
    assert_eq!(ArgValue::from(3_u16), ArgValue::Int(3));
    assert_eq!(ArgValue::from(3_u32), ArgValue::Int(3));
    assert_eq!(ArgValue::from(3_u64), ArgValue::Int(3));
    assert_eq!(ArgValue::from(3_usize), ArgValue::Int(3));
    assert_eq!(ArgValue::from(u64::MAX), ArgValue::Float(u64::MAX as f64));
  }

  #[test]
  fn args_set_replaces_and_converts_to_fluent() {
    let mut args = Args::new();
    assert!(args.is_empty());
    args.set("n", 1);
    args.set(String::from("n"), 2);
    let args = args.with("who", "Ada").with("ratio", 0.5);
    assert_eq!(args.len(), 3);
    assert_eq!(args.get("n"), Some(&ArgValue::Int(2)));
    assert_eq!(args.get("missing"), None);
    let fluent = args.to_fluent();
    assert_eq!(fluent.get("n"), Some(&FluentValue::from(2_i64)));
    assert_eq!(fluent.get("who"), Some(&FluentValue::from("Ada")));
    assert_eq!(fluent.get("ratio"), Some(&FluentValue::from(0.5_f64)));
  }
}
