//! Monthly statements: one CSV per account and month, stored in S3 and mailed to the account's
//! owner.
//!
//! [`crate::jobs`] runs them: the statement cron enqueues
//! [`StatementsDue`](crate::jobs::StatementsDue) for the previous month, its handler one
//! [`MonthlyStatement`](crate::jobs::MonthlyStatement) per open account (unique key
//! `account@month`), and each of those builds the statement:
//!
//! 1. the opening balance and the month's postings of the account, from the primary;
//! 2. the CSV ([`csv`]: `date,entry,memo,amount,balance`, RFC 4180 quoting), stored under
//!    `statements/<org>/<account>/<month>.csv` ([`object_key`]) — a rerun overwrites the same
//!    object;
//! 3. the mail, rendered from `templates/mail/statement.*`.
//!
//! # Attached or linked
//!
//! A statement of at most `statements.attach_max_bytes` (1 MiB) is **attached**: the mail goes
//! through the job queue (`deliver_later`), whose payload holds the attachment, and
//! id_effect_mail caps that at 2 MiB (`DeliverMail::MAX_CONTENT_BYTES`) because the payload stays
//! in the jobs table until it is pruned. A larger one is **linked**: the mail carries a presigned
//! S3 `GET` URL valid for `statements.link_ttl_hours` (7 days), so the queue row stays small and
//! the file is served by S3, not by the Ledger ([`Delivery::of`]). An account whose owner has no
//! address only stores its statement.

use std::fmt;
use std::time::Duration;

use id_effect::{UtcDateTime, ZonedDateTime};

/// A calendar month, `YYYY-MM`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Month {
  year: i32,
  month: u32,
}

impl Month {
  /// `year`-`month` (1–12), or `None`.
  pub fn new(year: i32, month: u32) -> Option<Self> {
    ((1..=12).contains(&month) && (1..=9999).contains(&year)).then_some(Self { year, month })
  }

  /// Parses `YYYY-MM`.
  pub fn parse(s: &str) -> Option<Self> {
    let (year, month) = s.split_once('-')?;
    if year.len() != 4 || month.len() != 2 {
      return None;
    }
    Self::new(year.parse().ok()?, month.parse().ok()?)
  }

  /// The month of `at`, in UTC.
  pub fn of(at: &ZonedDateTime) -> Self {
    let utc = UtcDateTime(at.inner().timestamp());
    Self {
      year: i32::from(utc.year()),
      month: u32::from(utc.month().unsigned_abs()),
    }
  }

  /// The month before.
  pub fn previous(self) -> Self {
    match self.month {
      1 => Self {
        year: self.year - 1,
        month: 12,
      },
      m => Self {
        year: self.year,
        month: m - 1,
      },
    }
  }

  /// The month after.
  pub fn next(self) -> Self {
    match self.month {
      12 => Self {
        year: self.year + 1,
        month: 1,
      },
      m => Self {
        year: self.year,
        month: m + 1,
      },
    }
  }

  /// Its first instant, RFC 3339 in UTC (`2026-09-01T00:00:00Z`).
  pub fn start(self) -> String {
    format!("{self}-01T00:00:00Z")
  }
}

impl fmt::Display for Month {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:04}-{:02}", self.year, self.month)
  }
}

/// One posting of a statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
  /// `YYYY-MM-DD`, UTC.
  pub date: String,
  /// The journal entry.
  pub entry_id: i64,
  /// Its memo.
  pub memo: String,
  /// Minor units, debit positive.
  pub amount: i64,
}

/// The statement CSV: a header, then each line with the running balance from `opening`.
pub fn csv(opening: i64, lines: &[Line]) -> String {
  let mut out = String::from("date,entry,memo,amount,balance\n");
  let mut balance = opening;
  for line in lines {
    balance += line.amount;
    out.push_str(&format!(
      "{},{},{},{},{}\n",
      line.date,
      line.entry_id,
      quote(&line.memo),
      line.amount,
      balance
    ));
  }
  out
}

/// `field` as a CSV field: quoted (quotes doubled) when it holds a comma, a quote or a line
/// break, and prefixed with `'` when a spreadsheet would read it as a formula.
fn quote(field: &str) -> String {
  let field = if field.starts_with(['=', '+', '-', '@']) {
    format!("'{field}")
  } else {
    field.to_owned()
  };
  if field.contains([',', '"', '\n', '\r']) {
    format!("\"{}\"", field.replace('"', "\"\""))
  } else {
    field
  }
}

/// Where the statement of `account` in `org` for `month` is stored.
pub fn object_key(org: &str, account: i64, month: Month) -> String {
  format!("statements/{org}/{account}/{month}.csv")
}

/// The file name the statement is attached as.
pub fn file_name(org: &str, code: &str, month: Month) -> String {
  format!("{org}-{code}-{month}.csv")
}

/// Minor units as `1,234.56` (`-` for a credit balance).
pub fn money(minor: i64) -> String {
  let sign = if minor < 0 { "-" } else { "" };
  let abs = minor.unsigned_abs();
  let units = (abs / 100).to_string();
  let mut grouped = String::new();
  for (i, digit) in units.chars().enumerate() {
    if i > 0 && (units.len() - i).is_multiple_of(3) {
      grouped.push(',');
    }
    grouped.push(digit);
  }
  format!("{sign}{grouped}.{:02}", abs % 100)
}

/// How a statement reaches its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
  /// The CSV is attached to the mail.
  Attached,
  /// The mail carries a presigned link to the CSV.
  Linked,
  /// The owner has no address: the statement is only stored.
  Stored,
}

impl Delivery {
  /// The delivery of a `bytes`-byte statement to an owner with (or without) an address, when
  /// statements up to `attach_max` bytes are attached ([module docs](self)).
  pub fn of(bytes: usize, has_address: bool, attach_max: usize) -> Self {
    match (has_address, bytes <= attach_max) {
      (false, _) => Self::Stored,
      (true, true) => Self::Attached,
      (true, false) => Self::Linked,
    }
  }

  /// The `statements.delivery` column value.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Attached => "attached",
      Self::Linked => "linked",
      Self::Stored => "stored",
    }
  }
}

/// `d` in words, for the link's lifetime (`7 days`, `12 hours`).
pub fn in_words(d: Duration) -> String {
  let hours = d.as_secs() / 3600;
  match hours {
    h if h >= 48 && h % 24 == 0 => format!("{} days", h / 24),
    1 => "1 hour".to_owned(),
    h => format!("{h} hours"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn months_parse_print_and_step() {
    let sep = Month::parse("2026-09").expect("a month");
    assert_eq!(sep.to_string(), "2026-09");
    assert_eq!(sep.next().to_string(), "2026-10");
    assert_eq!(
      Month::parse("2026-01").unwrap().previous().to_string(),
      "2025-12"
    );
    assert_eq!(
      Month::parse("2025-12").unwrap().next().to_string(),
      "2026-01"
    );
    assert_eq!(sep.start(), "2026-09-01T00:00:00Z");
    for bad in ["2026-13", "2026-9", "26-09", "2026/09", ""] {
      assert_eq!(Month::parse(bad), None, "{bad}");
    }
    let tick = ZonedDateTime::unsafe_make(1_788_242_400_000, id_effect::timezone::utc()); // 2026-09-01T06:00Z
    assert_eq!(Month::of(&tick).previous().to_string(), "2026-08");
  }

  #[test]
  fn the_csv_runs_the_balance_and_quotes_memos() {
    let line = |entry_id, memo: &str, amount| Line {
      date: "2026-09-02".into(),
      entry_id,
      memo: memo.into(),
      amount,
    };
    let text = csv(
      100,
      &[
        line(1, "rent, september", -40),
        line(2, "a \"quoted\" memo", 15),
        line(3, "=HYPERLINK(\"x\")", 5),
      ],
    );
    assert_eq!(
      text,
      "date,entry,memo,amount,balance\n\
       2026-09-02,1,\"rent, september\",-40,60\n\
       2026-09-02,2,\"a \"\"quoted\"\" memo\",15,75\n\
       2026-09-02,3,\"'=HYPERLINK(\"\"x\"\")\",5,80\n"
    );
  }

  #[test]
  fn small_statements_are_attached_large_ones_linked() {
    assert_eq!(Delivery::of(10, true, 1024), Delivery::Attached);
    assert_eq!(Delivery::of(1024, true, 1024), Delivery::Attached);
    assert_eq!(Delivery::of(1025, true, 1024), Delivery::Linked);
    assert_eq!(Delivery::of(10, false, 1024), Delivery::Stored);
    assert_eq!(Delivery::Linked.as_str(), "linked");
  }

  #[test]
  fn money_and_durations_read_well() {
    assert_eq!(money(0), "0.00");
    assert_eq!(money(12_345_678), "123,456.78");
    assert_eq!(money(-100_000), "-1,000.00");
    assert_eq!(in_words(Duration::from_secs(168 * 3600)), "7 days");
    assert_eq!(in_words(Duration::from_secs(12 * 3600)), "12 hours");
    assert_eq!(
      object_key("acme", 7, Month::parse("2026-09").unwrap()),
      "statements/acme/7/2026-09.csv"
    );
  }
}
