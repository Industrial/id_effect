//! Declarative redaction of sensitive fields and values, applied once before backend fan-out.

use std::borrow::Cow;

use ::id_effect::schema::Redacted;

use crate::LogRecord;

/// Placeholder written in place of a redacted value; matches the `Debug`/`Display` output of
/// [`Redacted`] and `id_effect_config::Secret`.
pub const DEFAULT_REDACTION_MASK: &str = "<redacted>";

/// Posture id of [`RedactionPolicy::dangerous_empty`] (ADR 0013 R10).
pub const DANGEROUS_EMPTY: &str = "id_effect_logger::RedactionPolicy::dangerous_empty";

/// Which parts of a [`LogRecord`] must never reach a backend.
///
/// - **Field names** ([`Self::field`]): an annotation whose key matches (ASCII case-insensitive)
///   has its value replaced by the mask.
/// - **Globs** ([`Self::glob`]): the same for keys matching a `*` / `?` pattern such as
///   `"*token*"` or `"password*"`.
/// - **Secret values** ([`Self::secret`], [`Self::redacted`]): every occurrence of a known secret
///   (an API key loaded through `Secret<T>`, a [`Redacted<T>`] value) is masked wherever it
///   appears — message, target, annotation values and span names.
///
/// Install it with [`crate::provide_redaction_policy`]; the logger applies it once per line,
/// before the composite fans out, so every backend (tracing, JSON, structured, OTel) sees the
/// same redacted record. A pipeline with no installed policy applies [`Self::recommended`]
/// (since 0.11, ADR 0013 R3): an installed policy replaces it, so build on
/// [`Self::recommended`] to keep those rules, or start from [`Self::dangerous_empty`] to pick
/// every rule yourself (a recorded opt-out). [`Self::dangerous_none`] turns redaction off and
/// records the opt-out in the posture registry.
///
/// ```
/// use id_effect_logger::{LogLevel, LogRecord, RedactionPolicy};
///
/// let policy = RedactionPolicy::dangerous_empty()
///   .field("password")
///   .glob("*token*")
///   .secret("sk-live-42");
/// let rec = LogRecord::new(LogLevel::Info, "charged with key sk-live-42")
///   .with_annotation("Password", "hunter2")
///   .with_annotation("refresh_token", "abc")
///   .with_annotation("user", "alice");
/// let out = policy.apply(&rec);
/// assert_eq!(out.message, "charged with key <redacted>");
/// assert_eq!(out.annotations["Password"], "<redacted>");
/// assert_eq!(out.annotations["refresh_token"], "<redacted>");
/// assert_eq!(out.annotations["user"], "alice");
/// ```
#[derive(Debug, Clone)]
pub struct RedactionPolicy {
  fields: Vec<String>,
  globs: Vec<String>,
  secrets: Vec<String>,
  mask: Cow<'static, str>,
}

/// Field names [`RedactionPolicy::recommended`] redacts (ASCII case-insensitive).
const RECOMMENDED_FIELDS: &[&str] = &[
  "password",
  "passwd",
  "secret",
  "token",
  "access_token",
  "refresh_token",
  "id_token",
  "api_key",
  "apikey",
  "authorization",
  "cookie",
  "set-cookie",
  "session",
  "csrf",
  "private_key",
  "client_secret",
];

/// Key globs [`RedactionPolicy::recommended`] redacts.
const RECOMMENDED_GLOBS: &[&str] = &["*_token", "*_secret", "*password*"];

/// [`RedactionPolicy::recommended`] since 0.11 (it was the empty policy before).
impl Default for RedactionPolicy {
  fn default() -> Self {
    Self::recommended()
  }
}

impl RedactionPolicy {
  /// A policy that redacts nothing, with [`DEFAULT_REDACTION_MASK`] as the mask.
  #[deprecated(
    since = "0.11.0",
    note = "redacts nothing: use `RedactionPolicy::recommended()` (the pipeline default), or \
            `RedactionPolicy::dangerous_empty()` to start from no rules"
  )]
  pub fn new() -> Self {
    Self::dangerous_empty()
  }

  /// A policy with no rules, with [`DEFAULT_REDACTION_MASK`] as the mask.
  #[deprecated(
    since = "0.11.0",
    note = "unrecorded opt-out from the recommended rules: use \
            `RedactionPolicy::recommended()` and add to it, or \
            `RedactionPolicy::dangerous_empty()` (recorded in the posture registry)"
  )]
  pub fn empty() -> Self {
    Self::dangerous_empty()
  }

  /// A policy with no rules, with [`DEFAULT_REDACTION_MASK`] as the mask: the explicit start
  /// for a policy whose every rule the caller picks. Installed as is, it redacts nothing.
  ///
  /// **Drops the recommended rules** (finding M11): a field such as `password` or
  /// `authorization` reaches every backend in clear unless the caller adds it back. Recorded in
  /// the [posture registry](id_effect::observability::posture) as [`DANGEROUS_EMPTY`]; build on
  /// [`Self::recommended`] to keep its rules without an opt-out.
  pub fn dangerous_empty() -> Self {
    ::id_effect::observability::posture::record_dangerous(
      DANGEROUS_EMPTY,
      "M11: log redaction starts from no rules; secret fields the caller does not add (password, \
       token, authorization, cookie) reach every log backend",
    );
    Self::no_rules()
  }

  /// No rules, unrecorded: the start of [`Self::recommended`].
  fn no_rules() -> Self {
    Self {
      fields: Vec::new(),
      globs: Vec::new(),
      secrets: Vec::new(),
      mask: Cow::Borrowed(DEFAULT_REDACTION_MASK),
    }
  }

  /// The rules a pipeline applies when no policy is installed: the fields `password`, `passwd`,
  /// `secret`, `token`, `access_token`, `refresh_token`, `id_token`, `api_key`, `apikey`,
  /// `authorization`, `cookie`, `set-cookie`, `session`, `csrf`, `private_key`, `client_secret`
  /// and the globs `*_token`, `*_secret`, `*password*` (all ASCII case-insensitive).
  pub fn recommended() -> Self {
    let policy = RECOMMENDED_FIELDS
      .iter()
      .fold(Self::no_rules(), |p, f| p.field(*f));
    RECOMMENDED_GLOBS.iter().fold(policy, |p, g| p.glob(*g))
  }

  /// A policy that redacts nothing, recorded as the posture opt-out
  /// `id_effect_logger::RedactionPolicy::dangerous_none`: installed, every field and value
  /// reaches every backend in clear.
  pub fn dangerous_none() -> Self {
    ::id_effect::observability::posture::record_dangerous(
      "id_effect_logger::RedactionPolicy::dangerous_none",
      "log redaction is off: passwords, tokens and cookies reach every log backend (M11)",
    );
    Self::no_rules()
  }

  /// Whether the policy has no rule at all (no field, glob or secret): it redacts nothing.
  pub fn is_empty(&self) -> bool {
    self.fields.is_empty() && self.globs.is_empty() && self.secrets.is_empty()
  }

  /// Redact the value of every annotation whose key equals `name` (ASCII case-insensitive).
  pub fn field(mut self, name: impl Into<String>) -> Self {
    self.fields.push(name.into().to_ascii_lowercase());
    self
  }

  /// Redact the value of every annotation whose key matches `pattern` (`*` = any run, `?` = one
  /// character; ASCII case-insensitive).
  pub fn glob(mut self, pattern: impl Into<String>) -> Self {
    self.globs.push(pattern.into().to_ascii_lowercase());
    self
  }

  /// Mask every occurrence of `value` in the message, target, annotation values and spans.
  ///
  /// Register the exposed form of a `Secret<T>` at startup: `policy.secret(key.expose())`.
  /// Empty strings are ignored.
  pub fn secret(mut self, value: impl Into<String>) -> Self {
    let value = value.into();
    if !value.is_empty() {
      self.secrets.push(value);
      // Longest first, so a secret that contains another is masked whole.
      self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
    self
  }

  /// [`Self::secret`] for a [`Redacted<T>`] value.
  pub fn redacted<T: AsRef<str>>(self, value: &Redacted<T>) -> Self {
    self.secret(value.expose().as_ref())
  }

  /// Replace the placeholder written in place of redacted values.
  pub fn mask(mut self, mask: impl Into<Cow<'static, str>>) -> Self {
    self.mask = mask.into();
    self
  }

  /// Whether an annotation key is redacted by a field name or glob rule.
  pub fn redacts_field(&self, key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    self.fields.contains(&key) || self.globs.iter().any(|g| glob_match(g, &key))
  }

  /// `rec` with every rule applied; borrowed unchanged when no rule matches.
  pub fn apply<'r, 'a>(&self, rec: &'r LogRecord<'a>) -> Cow<'r, LogRecord<'a>> {
    if !self.touches(rec) {
      return Cow::Borrowed(rec);
    }
    let mut out = rec.clone();
    if let Cow::Owned(m) = self.scrub(&out.message) {
      out.message = Cow::Owned(m);
    }
    if let Some(t) = out.target.as_ref()
      && let Cow::Owned(t) = self.scrub(t)
    {
      out.target = Some(Cow::Owned(t));
    }
    for (k, v) in out.annotations.iter_mut() {
      if self.redacts_field(k) {
        *v = self.mask.to_string();
      } else if let Cow::Owned(s) = self.scrub(v) {
        *v = s;
      }
    }
    for span in out.spans.iter_mut() {
      if let Cow::Owned(s) = self.scrub(span) {
        *span = s;
      }
    }
    Cow::Owned(out)
  }

  fn touches(&self, rec: &LogRecord<'_>) -> bool {
    let has_secret = |s: &str| self.secrets.iter().any(|x| s.contains(x.as_str()));
    has_secret(&rec.message)
      || rec.target.as_deref().is_some_and(has_secret)
      || rec
        .annotations
        .iter()
        .any(|(k, v)| (self.redacts_field(k) && *v != self.mask) || has_secret(v))
      || rec.spans.iter().any(|s| has_secret(s))
  }

  fn scrub<'s>(&self, s: &'s str) -> Cow<'s, str> {
    let mut out = Cow::Borrowed(s);
    for secret in &self.secrets {
      if out.contains(secret.as_str()) {
        out = Cow::Owned(out.replace(secret.as_str(), &self.mask));
      }
    }
    out
  }
}

/// `*` / `?` glob over ASCII-lowercased inputs.
fn glob_match(pattern: &str, text: &str) -> bool {
  let p: Vec<char> = pattern.chars().collect();
  let t: Vec<char> = text.chars().collect();
  let (mut pi, mut ti) = (0, 0);
  let (mut star, mut mark) = (None, 0);
  while ti < t.len() {
    if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
      pi += 1;
      ti += 1;
    } else if pi < p.len() && p[pi] == '*' {
      star = Some(pi);
      mark = ti;
      pi += 1;
    } else if let Some(s) = star {
      pi = s + 1;
      mark += 1;
      ti = mark;
    } else {
      return false;
    }
  }
  p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::LogLevel;
  use rstest::rstest;

  #[rstest]
  #[case("*token*", "refresh_token_v2", true)]
  #[case("password*", "password_hash", true)]
  #[case("password*", "old_password", false)]
  #[case("a?c", "abc", true)]
  #[case("a?c", "ac", false)]
  #[case("*", "", true)]
  #[case("", "x", false)]
  fn glob_match_cases(#[case] pattern: &str, #[case] text: &str, #[case] expected: bool) {
    assert_eq!(glob_match(pattern, text), expected);
  }

  #[test]
  fn untouched_record_is_borrowed() {
    let policy = RedactionPolicy::dangerous_empty()
      .field("password")
      .secret("s3cr3t");
    let rec = LogRecord::new(LogLevel::Info, "fine").with_annotation("user", "alice");
    assert!(matches!(policy.apply(&rec), Cow::Borrowed(_)));
  }

  #[test]
  fn secrets_masked_everywhere_longest_first() {
    let policy = RedactionPolicy::dangerous_empty()
      .secret("abc")
      .secret("abcdef")
      .secret("")
      .mask("***");
    let rec = LogRecord::new(LogLevel::Warn, "k=abcdef")
      .with_target("mod::abc")
      .with_annotation("note", "has abc")
      .with_spans(vec!["span-abc".into()]);
    let out = policy.apply(&rec);
    assert_eq!(out.message, "k=***");
    assert_eq!(out.target.as_deref(), Some("mod::***"));
    assert_eq!(out.annotations["note"], "has ***");
    assert_eq!(out.spans, vec!["span-***".to_string()]);
  }

  #[test]
  fn redacted_wrapper_registers_its_value() {
    let token = Redacted::new("tok-9".to_string());
    let policy = RedactionPolicy::default().redacted(&token);
    let rec = LogRecord::new(LogLevel::Info, "using tok-9");
    assert_eq!(policy.apply(&rec).message, "using <redacted>");
  }

  #[test]
  fn already_masked_field_is_borrowed() {
    let policy = RedactionPolicy::dangerous_empty().field("password");
    let rec = LogRecord::new(LogLevel::Info, "m").with_annotation("password", "<redacted>");
    assert!(matches!(policy.apply(&rec), Cow::Borrowed(_)));
    assert!(policy.redacts_field("PASSWORD"));
    assert!(!policy.redacts_field("user"));
  }

  #[test]
  fn redaction_opt_out_is_recorded() {
    let policy = RedactionPolicy::dangerous_empty();
    assert!(policy.is_empty());
    let recorded = ::id_effect::observability::posture::dangerous_opt_outs();
    assert!(
      recorded
        .iter()
        .any(|(id, reopens)| *id == DANGEROUS_EMPTY && reopens.contains("M11")),
      "{recorded:?}"
    );
  }

  #[test]
  #[allow(deprecated)]
  fn deprecated_empty_records_the_same_opt_out() {
    assert!(RedactionPolicy::empty().is_empty());
    assert!(
      ::id_effect::observability::posture::dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == DANGEROUS_EMPTY)
    );
  }
}
