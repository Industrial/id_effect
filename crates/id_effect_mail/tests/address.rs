//! `Address` validation.

use id_effect_mail::{Address, AddressError};

#[test]
fn splits_valid_addresses_at_the_at_sign() {
  let a = Address::parse("first.last+tag@mail.ledger.example").unwrap();
  assert_eq!(a.local(), "first.last+tag");
  assert_eq!(a.domain(), "mail.ledger.example");
  assert_eq!(a.as_str(), "first.last+tag@mail.ledger.example");
  assert!(Address::parse(format!("{}@x", "l".repeat(Address::MAX_LOCAL_LEN))).is_ok());
}

#[test]
fn rejects_malformed_addresses() {
  let cases = [
    ("carol", AddressError::MissingAt),
    ("@ledger.example", AddressError::EmptyLocal),
    ("carol @ledger.example", AddressError::Whitespace),
    ("carol@", AddressError::InvalidDomain),
    ("carol@ledger..example", AddressError::InvalidDomain),
    ("carol@ledger.example.", AddressError::InvalidDomain),
    ("carol@a@b", AddressError::InvalidDomain),
  ];
  for (addr, error) in cases {
    assert_eq!(Address::parse(addr), Err(error), "{addr}");
  }
  let long_local = format!("{}@x", "l".repeat(Address::MAX_LOCAL_LEN + 1));
  assert_eq!(Address::parse(long_local), Err(AddressError::LocalTooLong));
  let long = format!("carol@{}", "d.".repeat(125) + "x");
  assert_eq!(Address::parse(long), Err(AddressError::TooLong));
}
