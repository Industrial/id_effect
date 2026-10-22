//! Message formatting cost (0.13, `r13-e-benches`).
//!
//! - `i18n/format_plural`: [`Catalog::format`] of a plural message with a `count` argument, as a
//!   handler formats one line of a page: the arguments built for the call, the lookup through the
//!   fallback chain (`de-AT` → `de`), the CLDR plural selection and the number formatted into the
//!   text, with Unicode bidi isolation on (the default). One call per iteration; the count cycles
//!   through 1, 2, 5 and 21 so both plural branches are taken.
//!
//! Reported by `scripts/bench-compare.sh`; its budget (measured median × 1.5, 0.13 decision D15)
//! is `docs/benchmarks/gates-0.13.toml`, read against `docs/benchmarks/v0.13.json`
//! (`docs/benchmarks/README.md`, "Breadth gates (0.13)").
//!
//! Run: `cargo bench -p id_effect_i18n --bench format`

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect_i18n::{Args, Catalog, Locale};

const EN: &str = "\
inbox = { $count ->
    [one] You have one new message in { $folder }.
   *[other] You have { $count } new messages in { $folder }.
}
";

const DE: &str = "\
inbox = { $count ->
    [one] Sie haben eine neue Nachricht in { $folder }.
   *[other] Sie haben { $count } neue Nachrichten in { $folder }.
}
";

fn bench_format(c: &mut Criterion) {
  let en = Locale::parse("en").expect("en");
  let de = Locale::parse("de").expect("de");
  let catalog = Catalog::builder(en.clone())
    .embedded(en, "en/app.ftl", EN)
    .embedded(de, "de/app.ftl", DE)
    .build()
    .expect("every resource parses");
  let at = Locale::parse("de-AT").expect("de-AT");

  let format = |count: i64| {
    catalog.format(
      &at,
      "inbox",
      &Args::new()
        .with("count", count)
        .with("folder", "Posteingang"),
    )
  };
  // The message comes from `de` (the fallback of `de-AT`), in both plural forms.
  assert_eq!(
    format(1),
    "Sie haben eine neue Nachricht in \u{2068}Posteingang\u{2069}."
  );
  assert_eq!(
    format(21),
    "Sie haben \u{2068}21\u{2069} neue Nachrichten in \u{2068}Posteingang\u{2069}."
  );

  let counts = [1, 2, 5, 21];
  let mut n = 0usize;
  let mut group = c.benchmark_group("i18n");
  group.bench_function("format_plural", |b| {
    b.iter(|| {
      n = (n + 1) % counts.len();
      black_box(format(black_box(counts[n])))
    });
  });
  group.finish();
}

criterion_group!(benches, bench_format);
criterion_main!(benches);
