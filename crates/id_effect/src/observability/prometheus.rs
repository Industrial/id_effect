//! Prometheus text exposition (format 0.0.4) of a [`MetricRegistry`] snapshot, written by hand
//! (no `prometheus` dependency).
//!
//! | kind      | family (`# TYPE`)                    | samples                                                                    |
//! |-----------|--------------------------------------|----------------------------------------------------------------------------|
//! | counter   | `name` + `_total`, `counter`         | `name_total{tags} count`                                                   |
//! | gauge     | `name`, `gauge`                      | `name{tags} value`                                                         |
//! | histogram | `name`, `histogram`                  | cumulative `name_bucket{tags,le=".."}` ending with `le="+Inf"`, `name_sum`, `name_count` |
//! | summary   | `name`, `summary`                    | `name{tags,quantile=".."}` per quantile (`NaN` on an empty window), `name_sum`, `name_count` |
//! | frequency | `name` + `_total`, `counter`         | `name_total{tags,value=".."}` per observed value                           |
//!
//! `_total` is appended only when the name does not already end with it. Characters outside
//! the Prometheus name / label-name alphabets become `_` (and a leading digit gets a `_`
//! prefix); label values escape `\`, `"` and newlines. Families are sorted by name with one
//! `# TYPE` line each; when two kinds claim one family name, the first kind label in sort order
//! keeps it and the other's series are left out (a scrape must not declare a name twice). A tag
//! named like the sample's own label (`le`, `quantile`, `value`) is dropped from that sample.
//! No `# HELP` lines are written (metrics carry no description).
//!
//! ```
//! use id_effect::observability::prometheus;
//! use id_effect::{Metric, MetricRegistry};
//!
//! let registry = MetricRegistry::new();
//! registry.counter("requests").tagged("route", "/a").update_now(3);
//! assert_eq!(
//!   prometheus::render(&registry),
//!   "# TYPE requests_total counter\nrequests_total{route=\"/a\"} 3\n"
//! );
//! ```

use std::fmt::Write as _;

use super::metric::{MetricKind, MetricState};
use super::registry::{MetricPair, MetricRegistry};

/// The `Content-Type` of a [`render`]ed scrape body.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `registry`'s [`snapshot`](MetricRegistry::snapshot) in the text exposition format.
pub fn render(registry: &MetricRegistry) -> String {
  render_pairs(&registry.snapshot())
}

/// `pairs` (a registry snapshot, possibly with extra pairs such as
/// [`runtime_metrics::snapshot`](super::runtime_metrics::snapshot)) in the text exposition
/// format.
pub fn render_pairs(pairs: &[MetricPair]) -> String {
  let mut rows: Vec<(String, &MetricPair)> = pairs.iter().map(|p| (family_name(p), p)).collect();
  rows.sort_by(|(fa, a), (fb, b)| {
    (fa.as_str(), a.key.kind().label(), a.key.tags()).cmp(&(
      fb.as_str(),
      b.key.kind().label(),
      b.key.tags(),
    ))
  });
  let mut out = String::new();
  let mut current: Option<(&str, &str)> = None;
  for (family, pair) in &rows {
    let kind = pair.key.kind().label();
    match current {
      Some((f, k)) if f == family.as_str() => {
        if k != kind {
          continue;
        }
      }
      _ => {
        let _ = writeln!(out, "# TYPE {family} {}", type_name(pair.key.kind()));
        current = Some((family.as_str(), kind));
      }
    }
    write_samples(&mut out, family, pair);
  }
  out
}

/// The family name of `pair`: the sanitized name, plus `_total` for counters and frequencies.
fn family_name(pair: &MetricPair) -> String {
  let name = sanitize(pair.key.name(), true);
  match pair.key.kind() {
    MetricKind::Counter | MetricKind::Frequency if !name.ends_with("_total") => name + "_total",
    _ => name,
  }
}

fn type_name(kind: &MetricKind) -> &'static str {
  match kind {
    MetricKind::Counter | MetricKind::Frequency => "counter",
    MetricKind::Gauge => "gauge",
    MetricKind::Histogram(_) => "histogram",
    MetricKind::Summary(_) => "summary",
  }
}

fn write_samples(out: &mut String, family: &str, pair: &MetricPair) {
  let tags = pair.key.tags();
  match &pair.state {
    MetricState::Counter { count } => sample(out, family, "", tags, None, &count.to_string()),
    MetricState::Gauge { value } => sample(out, family, "", tags, None, &value.to_string()),
    MetricState::Histogram(h) => {
      // `HistogramSnapshot::buckets` are per bucket; the exposition format wants them cumulative.
      let mut cumulative = 0_u64;
      for (bound, n) in &h.buckets {
        cumulative = cumulative.saturating_add(*n);
        let le = float(*bound);
        sample(
          out,
          family,
          "_bucket",
          tags,
          Some(("le", &le)),
          &cumulative.to_string(),
        );
      }
      if h.buckets.last().is_none_or(|(b, _)| *b != f64::INFINITY) {
        sample(
          out,
          family,
          "_bucket",
          tags,
          Some(("le", "+Inf")),
          &h.count.to_string(),
        );
      }
      sample(out, family, "_sum", tags, None, &float(h.sum));
      sample(out, family, "_count", tags, None, &h.count.to_string());
    }
    MetricState::Summary(s) => {
      for (q, v) in &s.quantiles {
        let q = float(*q);
        let v = float(v.unwrap_or(f64::NAN));
        sample(out, family, "", tags, Some(("quantile", &q)), &v);
      }
      sample(out, family, "_sum", tags, None, &float(s.sum));
      sample(out, family, "_count", tags, None, &s.count.to_string());
    }
    MetricState::Frequency { occurrences } => {
      for (value, n) in occurrences {
        sample(
          out,
          family,
          "",
          tags,
          Some(("value", value)),
          &n.to_string(),
        );
      }
    }
  }
}

/// One sample line: `family suffix {tags, extra} value`.
fn sample(
  out: &mut String,
  family: &str,
  suffix: &str,
  tags: &[(String, String)],
  extra: Option<(&str, &str)>,
  value: &str,
) {
  out.push_str(family);
  out.push_str(suffix);
  let mut open = false;
  let extra_key = extra.map(|(k, _)| k);
  let labels = tags
    .iter()
    .map(|(k, v)| (k.as_str(), v.as_str()))
    .filter(|(k, _)| Some(*k) != extra_key)
    .chain(extra);
  for (k, v) in labels {
    out.push(if open { ',' } else { '{' });
    open = true;
    out.push_str(&sanitize(k, false));
    out.push_str("=\"");
    escape_into(out, v);
    out.push('"');
  }
  if open {
    out.push('}');
  }
  out.push(' ');
  out.push_str(value);
  out.push('\n');
}

/// `name` over `[a-zA-Z0-9_]` (plus `:` for metric names), not starting with a digit.
fn sanitize(name: &str, colon: bool) -> String {
  let mut s = String::with_capacity(name.len() + 1);
  for (i, c) in name.chars().enumerate() {
    if i == 0 && c.is_ascii_digit() {
      s.push('_');
    }
    let ok = c.is_ascii_alphanumeric() || c == '_' || (colon && c == ':');
    s.push(if ok { c } else { '_' });
  }
  if s.is_empty() {
    s.push('_');
  }
  s
}

/// A label value with `\`, `"` and newlines escaped.
fn escape_into(out: &mut String, v: &str) {
  for c in v.chars() {
    match c {
      '\\' => out.push_str("\\\\"),
      '"' => out.push_str("\\\""),
      '\n' => out.push_str("\\n"),
      c => out.push(c),
    }
  }
}

/// A sample value or bound: `+Inf`, `-Inf`, `NaN` or the shortest decimal.
fn float(v: f64) -> String {
  if v.is_nan() {
    "NaN".to_owned()
  } else if v == f64::INFINITY {
    "+Inf".to_owned()
  } else if v == f64::NEG_INFINITY {
    "-Inf".to_owned()
  } else {
    v.to_string()
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;
  use crate::observability::{Boundaries, Metric, MetricKey, SummaryConfig};

  #[test]
  fn renders_every_kind_in_exposition_format_0_0_4() {
    let r = MetricRegistry::new();
    r.counter("requests").tagged("route", "/a").update_now(3);
    r.counter("requests")
      .tagged("route", "q\"\\\n")
      .update_now(1);
    r.counter("jobs_total").update_now(1);
    r.gauge("temp").update_now(-2);
    let h = r.histogram("latency", Boundaries::new([0.1, 0.5]));
    for v in [0.0625, 0.25, 0.25, 2.0] {
      h.update_now(v);
    }
    let s = r.summary(
      "size",
      SummaryConfig::new(Duration::from_secs(60), 10, [0.5, 1.0]),
    );
    for v in [1.0, 2.0, 3.0] {
      s.update_now(v);
    }
    let f = r.frequency("status");
    for v in ["ok", "err", "ok"] {
      f.update_now(v.to_owned());
    }
    let expected = r#"# TYPE jobs_total counter
jobs_total 1
# TYPE latency histogram
latency_bucket{le="0.1"} 1
latency_bucket{le="0.5"} 3
latency_bucket{le="+Inf"} 4
latency_sum 2.5625
latency_count 4
# TYPE requests_total counter
requests_total{route="/a"} 3
requests_total{route="q\"\\\n"} 1
# TYPE size summary
size{quantile="0.5"} 2
size{quantile="1"} 3
size_sum 6
size_count 3
# TYPE status_total counter
status_total{value="err"} 1
status_total{value="ok"} 2
# TYPE temp gauge
temp -2
"#;
    assert_eq!(render(&r), expected);
  }

  #[test]
  fn tags_precede_the_sample_label_and_names_are_sanitized() {
    let r = MetricRegistry::new();
    let h = r
      .histogram("http.latency-s", Boundaries::new([1.0]))
      .tagged("le", "dropped")
      .tagged("0route", "/x");
    h.update_now(0.5);
    let s = r.summary(
      "empty",
      SummaryConfig::new(Duration::from_secs(1), 4, [0.99]),
    );
    let _ = s.state();
    let expected = r#"# TYPE empty summary
empty{quantile="0.99"} NaN
empty_sum 0
empty_count 0
# TYPE http_latency_s histogram
http_latency_s_bucket{_0route="/x",le="1"} 1
http_latency_s_bucket{_0route="/x",le="+Inf"} 1
http_latency_s_sum{_0route="/x",le="dropped"} 0.5
http_latency_s_count{_0route="/x",le="dropped"} 1
"#;
    assert_eq!(render(&r), expected);
  }

  #[test]
  fn one_family_per_name_and_extra_pairs_merge_in_order() {
    let r = MetricRegistry::new();
    r.counter("x").update_now(1);
    r.frequency("x").update_now("a".to_owned());
    let mut pairs = r.snapshot();
    pairs.push(MetricPair {
      key: MetricKey::new("a_gauge", MetricKind::Gauge),
      state: MetricState::Gauge { value: 7 },
    });
    assert_eq!(
      render_pairs(&pairs),
      "# TYPE a_gauge gauge\na_gauge 7\n# TYPE x_total counter\nx_total 1\n"
    );
  }

  #[test]
  fn floats_render_like_prometheus() {
    assert_eq!(float(f64::INFINITY), "+Inf");
    assert_eq!(float(f64::NEG_INFINITY), "-Inf");
    assert_eq!(float(f64::NAN), "NaN");
    assert_eq!(float(0.005), "0.005");
    assert_eq!(float(10.0), "10");
    assert_eq!(sanitize("", true), "_");
    assert_eq!(sanitize("a:b", true), "a:b");
    assert_eq!(sanitize("a:b", false), "a_b");
    assert!(CONTENT_TYPE.starts_with("text/plain; version=0.0.4"));
  }
}
