//! Outgoing mail for id_effect services: SMTP, the SendGrid v3 API, templates, and delivery through
//! `id_effect_jobs`.
//!
//! [`Address`] is a validated `local@domain` mailbox address: no whitespace or control characters,
//! a local part of at most [`Address::MAX_LOCAL_LEN`] bytes, a domain of non-empty dot-separated
//! labels, and at most [`Address::MAX_LEN`] bytes in all (RFC 5321).
//!
//! ```
//! use id_effect_mail::{Address, AddressError};
//!
//! let from = Address::parse("billing@ledger.example").unwrap();
//! assert_eq!(from.to_string(), "billing@ledger.example");
//! assert_eq!(Address::parse("billing"), Err(AddressError::MissingAt));
//! ```
//!
//! Tier `integration` (ADR 0010 Amendment 6).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

/// A validated `local@domain` mailbox address.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Address {
  addr: String,
  at: usize,
}

impl Address {
  /// Longest address, in bytes (RFC 5321 path limit minus the angle brackets).
  pub const MAX_LEN: usize = 254;
  /// Longest local part, in bytes (RFC 5321).
  pub const MAX_LOCAL_LEN: usize = 64;

  /// Validates `addr` as a mailbox address.
  pub fn parse(addr: impl Into<String>) -> Result<Self, AddressError> {
    let addr = addr.into();
    if addr.chars().any(|c| c.is_whitespace() || c.is_control()) {
      return Err(AddressError::Whitespace);
    }
    let (local, domain) = addr.split_once('@').ok_or(AddressError::MissingAt)?;
    if local.is_empty() {
      return Err(AddressError::EmptyLocal);
    }
    if local.len() > Self::MAX_LOCAL_LEN {
      return Err(AddressError::LocalTooLong);
    }
    if domain.contains('@') || domain.split('.').any(str::is_empty) {
      return Err(AddressError::InvalidDomain);
    }
    if addr.len() > Self::MAX_LEN {
      return Err(AddressError::TooLong);
    }
    let at = local.len();
    Ok(Address { addr, at })
  }

  /// The part before the `@`.
  pub fn local(&self) -> &str {
    &self.addr[..self.at]
  }

  /// The part after the `@`.
  pub fn domain(&self) -> &str {
    &self.addr[self.at + 1..]
  }

  /// The whole address.
  pub fn as_str(&self) -> &str {
    &self.addr
  }
}

impl fmt::Display for Address {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.addr)
  }
}

/// Why a string is not a valid [`Address`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressError {
  /// It contains whitespace or a control character.
  Whitespace,
  /// It has no `@`.
  MissingAt,
  /// Nothing comes before the `@`.
  EmptyLocal,
  /// The local part is longer than [`Address::MAX_LOCAL_LEN`] bytes.
  LocalTooLong,
  /// The domain is empty, has an empty label (`a..b`, a leading or trailing dot) or another `@`.
  InvalidDomain,
  /// The address is longer than [`Address::MAX_LEN`] bytes.
  TooLong,
}

impl fmt::Display for AddressError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      AddressError::Whitespace => "mail address contains whitespace or a control character",
      AddressError::MissingAt => "mail address has no `@`",
      AddressError::EmptyLocal => "mail address has nothing before the `@`",
      AddressError::LocalTooLong => "mail address local part is longer than 64 bytes",
      AddressError::InvalidDomain => "mail address domain is not a dot-separated host name",
      AddressError::TooLong => "mail address is longer than 254 bytes",
    })
  }
}

impl std::error::Error for AddressError {}
