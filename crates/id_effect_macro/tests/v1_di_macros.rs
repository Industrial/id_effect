//! Expansion behaviour of the v1 capability-DI macros (`caps!`, `provide!`, `providers!`,
//! `mock_capability!`) against the v1 `::id_effect` capability types.
//!
//! These macros expand to v1-kernel types and are dropped together with the v1 kernel
//! (k-retire-v1 part 2b, see `docs/migration-0.7/k-retire-v1.md`); this file goes with them.

use id_effect::{Cap, CapList, Env, build_env};
use id_effect_macro::{caps, mock_capability, provide, providers};

#[derive(Clone, Debug, PartialEq)]
struct Counter(u32);

#[derive(Clone, Debug, PartialEq)]
struct Name(&'static str);

mock_capability!(CounterMock, Counter, "counter/mock", || Counter(7));
mock_capability!(NameMock, Name, "name/mock", || Name("ada"));

#[test]
fn caps_with_no_keys_is_plain_env() {
  let env: caps!() = Env::new();
  assert!(env.is_empty());
}

#[test]
fn caps_expands_to_cap_list_of_service_slots() {
  let list: caps!(Counter, Name) = CapList::new(Env::new());
  let _same_type: CapList<(Cap<Counter>, Cap<Name>)> = list;
}

#[test]
fn provide_and_mock_capability_build_an_env() {
  let env = build_env([provide!(CounterMock)]).expect("env");
  assert_eq!(env.get::<Cap<Counter>>(), &Counter(7));
}

#[test]
fn providers_expands_to_provider_array() {
  let bundle = providers!(dev: [CounterMock, NameMock]);
  assert_eq!(bundle.len(), 2);
  let env = build_env(bundle).expect("env");
  assert_eq!(env.get::<Cap<Counter>>(), &Counter(7));
  assert_eq!(env.get::<Cap<Name>>(), &Name("ada"));
}

#[test]
fn providers_accepts_trailing_comma_and_empty_bundle() {
  let one = providers!(single: [NameMock,]);
  assert_eq!(one.len(), 1);
  let none: [id_effect::ProviderBox; 0] = providers!(empty: []);
  assert!(build_env(none).expect("empty env").is_empty());
}
