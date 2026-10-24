//! `Catalog` acceptance tests (r13-e9-4-i18n-core): eco.i18n #1 (catalogs, fallback chain,
//! startup check) and #2 (CLDR plural categories through Fluent selectors), and the R4 loading
//! rules (allowlisted directories only, parse errors fail startup).

use std::path::{Path, PathBuf};

use id_effect_i18n::{
  Args, Catalog, CatalogError, LanguageIdentifier, Locale, MAX_RESOURCE_BYTES, MissingKey,
  Translation,
};

fn loc(tag: &str) -> Locale {
  Locale::parse(tag).expect("test locale")
}

fn fixtures() -> PathBuf {
  Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/locales")
}

/// The fixture catalog (`tests/fixtures/locales`), default `en`, plain output.
fn fixture_catalog() -> Catalog {
  Catalog::builder(loc("en"))
    .allow_dir(fixtures())
    .and_then(|b| b.dir(fixtures()))
    .expect("fixtures load")
    .use_isolating(false)
    .build()
    .expect("fixtures parse")
}

/// A fresh scratch directory under cargo's per-test-target temp dir.
fn scratch(name: &str) -> PathBuf {
  let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
    .join("id_effect_i18n")
    .join(format!("{name}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).expect("scratch dir");
  dir
}

fn count(n: impl Into<id_effect_i18n::ArgValue>) -> Args {
  Args::new().with("count", n)
}

#[test]
fn plural_categories_en_de_pl_ar_ja() {
  let catalog = fixture_catalog();
  let category = |tag: &str, n: f64| catalog.format(&loc(tag), "items", &count(n));
  let cases: &[(&str, &[(f64, &str)])] = &[
    (
      "en",
      &[(0.0, "other"), (1.0, "one"), (2.0, "other"), (1.5, "other")],
    ),
    (
      "de",
      &[
        (0.0, "other"),
        (1.0, "one"),
        (2.0, "other"),
        (21.0, "other"),
      ],
    ),
    (
      "pl",
      &[
        (1.0, "one"),
        (2.0, "few"),
        (4.0, "few"),
        (5.0, "many"),
        (12.0, "many"),
        (22.0, "few"),
        (25.0, "many"),
        (1.5, "other"),
      ],
    ),
    (
      "ar",
      &[
        (0.0, "zero"),
        (1.0, "one"),
        (2.0, "two"),
        (3.0, "few"),
        (10.0, "few"),
        (11.0, "many"),
        (99.0, "many"),
        (100.0, "other"),
        (102.0, "other"),
      ],
    ),
    ("ja", &[(0.0, "other"), (1.0, "other"), (2.0, "other")]),
  ];
  for (tag, expected) in cases {
    for (n, want) in *expected {
      assert_eq!(category(tag, *n), *want, "{tag} {n}");
    }
  }
  // Integer arguments select the same way, and the selected variant interpolates the number.
  assert_eq!(catalog.format(&loc("en"), "files", &count(1)), "1 file");
  assert_eq!(
    catalog.format(&loc("en"), "files", &count(7_u64)),
    "7 files"
  );
  assert_eq!(catalog.format(&loc("de"), "files", &count(1)), "1 Datei");
  assert_eq!(catalog.format(&loc("pl"), "files", &count(3)), "3 pliki");
  assert_eq!(catalog.format(&loc("pl"), "files", &count(5)), "5 plików");
  // A regional locale uses its language's rules: de-AT has no `items`, de's selects.
  assert_eq!(catalog.format(&loc("de-AT"), "items", &count(1)), "one");
}

#[test]
fn missing_key_falls_back_to_default_locale() {
  let catalog = fixture_catalog();
  let name = Args::new().with("name", "Ada");
  let de_at = loc("de-AT");

  // requested → language → default
  assert_eq!(
    catalog.fallback_chain(&de_at),
    [&loc("de-AT"), &loc("de"), &loc("en")]
  );
  assert_eq!(
    catalog.lookup(&de_at, "greeting", &name),
    Some(Translation {
      text: "Servus, Ada!".into(),
      locale: loc("de-AT"),
    })
  );
  let from_de = catalog
    .lookup(&de_at, "files", &count(2))
    .expect("de has files");
  assert_eq!(
    (from_de.text.as_str(), from_de.locale),
    ("2 Dateien", loc("de"))
  );
  let from_en = catalog
    .lookup(&de_at, "farewell", &Args::new())
    .expect("en has farewell");
  assert_eq!(
    (from_en.text.as_str(), from_en.locale),
    ("Goodbye from Pipeline.", loc("en"))
  );

  // Attributes fall back separately from their message.
  assert_eq!(catalog.format(&loc("de"), "form", &Args::new()), "Formular");
  assert_eq!(
    catalog.format(&loc("de"), "form.title", &Args::new()),
    "Order form"
  );
  assert_eq!(
    catalog.format(&loc("de"), "form.nope", &Args::new()),
    "form.nope"
  );

  // A locale the catalog does not have starts the chain at its language or the default.
  assert_eq!(catalog.fallback_chain(&loc("fr-CA")), [&loc("en")]);
  assert_eq!(
    catalog.format(&loc("fr-CA"), "greeting", &name),
    "Hello, Ada!"
  );
  assert_eq!(catalog.fallback_chain(&loc("en")), [&loc("en")]);

  // A key no locale has: `lookup` says so, `format` shows the key.
  assert_eq!(catalog.lookup(&de_at, "nope", &Args::new()), None);
  assert_eq!(catalog.format(&de_at, "nope", &Args::new()), "nope");
  // A message without a value (attributes only) has no text of its own.
  let attrs_only = Catalog::builder(loc("en"))
    .source(loc("en"), "inline", "menu =\n    .label = Menu\n")
    .build()
    .expect("parses");
  assert_eq!(attrs_only.lookup(&loc("en"), "menu", &Args::new()), None);
  assert_eq!(
    attrs_only.format(&loc("en"), "menu.label", &Args::new()),
    "Menu"
  );
  // A missing argument renders as Fluent's placeholder instead of failing.
  assert_eq!(
    catalog.format(&loc("en"), "greeting", &Args::new()),
    "Hello, {$name}!"
  );
}

#[test]
fn catalog_check_reports_missing_keys() {
  let catalog = fixture_catalog();
  let report = catalog.check();
  assert!(!report.is_complete());
  let missing = |tag: &str| {
    let locale = loc(tag);
    report
      .missing_in(&locale)
      .map(str::to_owned)
      .collect::<Vec<_>>()
  };
  assert_eq!(missing("de"), ["farewell", "form.title"]);
  // de-AT is covered by de where de has the key; what de lacks, de-AT lacks too.
  assert_eq!(missing("de-AT"), ["farewell", "form.title"]);
  assert_eq!(
    missing("pl"),
    ["farewell", "form", "form.title", "greeting"]
  );
  assert_eq!(
    missing("ar"),
    ["farewell", "files", "form", "form.title", "greeting"]
  );
  assert_eq!(
    missing("ja"),
    ["farewell", "files", "form", "form.title", "greeting"]
  );
  assert!(
    missing("en").is_empty(),
    "the default locale is the reference"
  );
  assert_eq!(report.missing().len(), 18);
  assert_eq!(
    report.missing()[0],
    MissingKey {
      locale: loc("ar"),
      key: "farewell".into(),
    }
  );
  assert_eq!(report.missing()[0].to_string(), "ar: farewell");
  let text = report.to_string();
  assert!(
    text.starts_with("18 missing key(s): ar: farewell;"),
    "{text}"
  );

  // `require_complete` turns the report into a startup failure.
  let strict = Catalog::builder(loc("en"))
    .allow_dir(fixtures())
    .and_then(|b| b.dir(fixtures()))
    .expect("fixtures load")
    .require_complete()
    .build();
  match strict {
    Err(CatalogError::Incomplete(r)) => assert_eq!(r, report),
    other => panic!("expected Incomplete, got {other:?}"),
  }

  // A complete catalog passes both.
  let complete = Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "a = A\nb = B\n    .x = X")
    .embedded(loc("de"), "de.ftl", "a = A\nb = B\n    .x = X")
    .embedded(loc("de-CH"), "de-CH.ftl", "a = A")
    .require_complete()
    .build()
    .expect("complete");
  let report = complete.check();
  assert!(report.is_complete());
  assert!(report.missing().is_empty());
  assert_eq!(
    report.to_string(),
    "every locale has every key of the default locale"
  );
  assert!(complete.has_key(&loc("de"), "b.x"));
  assert!(!complete.has_key(&loc("de-CH"), "b.x"));
  assert!(!complete.has_key(&loc("fr"), "a"));
}

#[test]
fn catalog_rejects_path_outside_allowlist() {
  let root = scratch("allowlist");
  let allowed = root.join("locales");
  let outside = root.join("secrets");
  std::fs::create_dir_all(allowed.join("en")).expect("mkdir");
  std::fs::create_dir_all(&outside).expect("mkdir");
  std::fs::write(allowed.join("en/app.ftl"), "hello = Hello").expect("write");
  std::fs::write(outside.join("leak.ftl"), "secret = s3cr3t").expect("write");

  let builder = || {
    Catalog::builder(loc("en"))
      .allow_dir(&allowed)
      .expect("allow")
  };
  let refused = |r: Result<_, CatalogError>| match r {
    Err(CatalogError::PathOutsideAllowlist { path }) => path,
    Err(other) => panic!("expected PathOutsideAllowlist, got {other}"),
    Ok(_) => panic!("expected PathOutsideAllowlist, got a builder"),
  };

  // A file outside every allowlisted directory, directly or through `..`.
  let direct = outside.join("leak.ftl");
  assert_eq!(refused(builder().file(loc("en"), &direct)), direct);
  let dotdot = allowed.join("../secrets/leak.ftl");
  assert_eq!(refused(builder().file(loc("en"), &dotdot)), dotdot);
  // A directory outside.
  assert_eq!(refused(builder().dir(&outside)), outside);
  // Nothing allowlisted: every path is refused, even an existing catalog file.
  let inside = allowed.join("en/app.ftl");
  assert_eq!(
    refused(Catalog::builder(loc("en")).file(loc("en"), &inside)),
    inside
  );

  // A symbolic link inside the allowlisted directory that points outside it.
  #[cfg(unix)]
  {
    std::os::unix::fs::symlink(&direct, allowed.join("en/link.ftl")).expect("symlink");
    assert!(matches!(
      builder().dir(&allowed),
      Err(CatalogError::PathOutsideAllowlist { .. })
    ));
    std::fs::remove_file(allowed.join("en/link.ftl")).expect("unlink");
  }

  // The allowlisted file itself loads.
  let catalog = builder()
    .file(loc("en"), &inside)
    .expect("inside")
    .build()
    .expect("parses");
  assert_eq!(catalog.format(&loc("en"), "hello", &Args::new()), "Hello");
  let error = CatalogError::PathOutsideAllowlist { path: direct };
  assert!(error.to_string().contains("outside the allowlisted"));
}

#[test]
fn catalog_build_fails_on_parse_error_conflict_and_missing_default() {
  let parse = Catalog::builder(loc("en"))
    .embedded(loc("en"), "broken.ftl", "ok = fine\nbroken = { $\n")
    .build();
  match parse {
    Err(CatalogError::Parse {
      locale,
      origin,
      errors,
    }) => {
      assert_eq!((locale, origin.as_str()), (loc("en"), "broken.ftl"));
      assert!(!errors.is_empty());
    }
    other => panic!("expected Parse, got {other:?}"),
  }

  let conflict = Catalog::builder(loc("en"))
    .embedded(loc("en"), "a.ftl", "hello = A")
    .embedded(loc("en"), "b.ftl", "hello = B")
    .build();
  match conflict {
    Err(CatalogError::Conflict {
      locale,
      origin,
      errors,
    }) => {
      assert_eq!((locale, origin.as_str()), (loc("en"), "b.ftl"));
      assert_eq!(errors.len(), 1);
    }
    other => panic!("expected Conflict, got {other:?}"),
  }

  let no_default = Catalog::builder(loc("en"))
    .embedded(loc("de"), "de.ftl", "hello = Hallo")
    .build();
  assert_eq!(
    no_default.map(|_| ()),
    Err(CatalogError::MissingDefault { locale: loc("en") })
  );
}

#[test]
fn catalog_refuses_unreadable_oversize_and_foreign_files() {
  let root = scratch("files");
  std::fs::create_dir_all(root.join("en")).expect("mkdir");
  let builder = || Catalog::builder(loc("en")).allow_dir(&root).expect("allow");

  // Missing allowlist directory and missing file: Io, naming the path as given.
  match Catalog::builder(loc("en")).allow_dir(root.join("nope")) {
    Err(CatalogError::Io { path, message }) => {
      assert_eq!(path, root.join("nope"));
      assert!(!message.is_empty());
    }
    other => panic!("expected Io, got {other:?}"),
  }
  assert!(matches!(
    builder().file(loc("en"), root.join("en/missing.ftl")),
    Err(CatalogError::Io { .. })
  ));

  // Wrong extension.
  std::fs::write(root.join("en/app.txt"), "hello = Hello").expect("write");
  assert_eq!(
    builder()
      .file(loc("en"), root.join("en/app.txt"))
      .map(|_| ()),
    Err(CatalogError::NotFluentFile {
      path: root.join("en/app.txt"),
    })
  );

  // Larger than the limit.
  let big = root.join("en/big.ftl");
  let line = "k = v\n";
  let lines = usize::try_from(MAX_RESOURCE_BYTES).expect("fits") / line.len() + 1;
  std::fs::write(&big, line.repeat(lines)).expect("write");
  assert_eq!(
    builder().file(loc("en"), &big).map(|_| ()),
    Err(CatalogError::TooLarge {
      path: big.clone(),
      limit: MAX_RESOURCE_BYTES,
    })
  );
  std::fs::remove_file(&big).expect("rm");

  // Not UTF-8.
  let latin1 = root.join("en/latin1.ftl");
  std::fs::write(&latin1, b"hello = gr\xfc\xdf").expect("write");
  assert!(matches!(
    builder().file(loc("en"), &latin1),
    Err(CatalogError::Io { .. })
  ));
  std::fs::remove_file(&latin1).expect("rm");

  // A directory with the .ftl extension cannot be read as a resource.
  std::fs::create_dir_all(root.join("en/dir.ftl")).expect("mkdir");
  assert!(matches!(
    builder().file(loc("en"), root.join("en/dir.ftl")),
    Err(CatalogError::Io { .. })
  ));
  std::fs::remove_dir(root.join("en/dir.ftl")).expect("rmdir");

  // A subdirectory that is not a locale.
  std::fs::create_dir_all(root.join("not a locale")).expect("mkdir");
  assert_eq!(
    builder().dir(&root).map(|_| ()),
    Err(CatalogError::InvalidLocaleDir {
      path: std::fs::canonicalize(&root)
        .expect("canon")
        .join("not a locale"),
    })
  );

  // An allowlisted path that is a file, not a directory, cannot be listed.
  std::fs::write(root.join("en/app.ftl"), "hello = Hello").expect("write");
  assert!(matches!(
    builder().dir(root.join("en/app.ftl")),
    Err(CatalogError::Io { .. })
  ));
}

#[test]
fn catalog_errors_display_their_cause() {
  let errors = [
    CatalogError::Io {
      path: "a.ftl".into(),
      message: "denied".into(),
    },
    CatalogError::PathOutsideAllowlist {
      path: "/etc/x.ftl".into(),
    },
    CatalogError::NotFluentFile {
      path: "a.txt".into(),
    },
    CatalogError::TooLarge {
      path: "big.ftl".into(),
      limit: 9,
    },
    CatalogError::InvalidLocaleDir { path: "x y".into() },
    CatalogError::Parse {
      locale: loc("en"),
      origin: "a.ftl".into(),
      errors: vec!["e1".into(), "e2".into()],
    },
    CatalogError::Conflict {
      locale: loc("en"),
      origin: "b.ftl".into(),
      errors: vec!["dup".into()],
    },
    CatalogError::MissingDefault { locale: loc("en") },
    CatalogError::Incomplete(Default::default()),
  ];
  let expected = [
    "cannot read a.ftl: denied",
    "/etc/x.ftl is outside the allowlisted catalog directories",
    "a.txt is not a .ftl file",
    "big.ftl is larger than 9 bytes",
    "x y is not named after a locale",
    "Fluent syntax error in a.ftl (en): e1; e2",
    "b.ftl (en) redefines messages: dup",
    "the default locale en has no resource",
    "incomplete catalog: every locale has every key of the default locale",
  ];
  for (error, want) in errors.iter().zip(expected) {
    assert_eq!(error.to_string(), want);
    let _: &dyn std::error::Error = error;
  }
}

#[test]
fn catalog_isolates_arguments_by_default_and_reports_its_locales() {
  let catalog = Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "hello = Hello, { $name }!")
    .embedded(loc("ar"), "ar.ftl", "hello = مرحبا { $name }")
    .build()
    .expect("parses");
  let args = Args::new().with("name", "Ada");
  assert_eq!(
    catalog.format(&loc("en"), "hello", &args),
    "Hello, \u{2068}Ada\u{2069}!"
  );
  assert_eq!(catalog.default_locale(), &loc("en"));
  assert_eq!(
    catalog.locales().collect::<Vec<_>>(),
    [&loc("ar"), &loc("en")]
  );
  assert!(catalog.supports(&loc("ar")));
  assert!(!catalog.supports(&loc("ar-EG")));
  let debug = format!("{catalog:?}");
  assert!(debug.contains("Catalog") && debug.contains("ar"), "{debug}");
  let id = LanguageIdentifier::from(&loc("ar"));
  assert_eq!(id.language.as_str(), "ar");

  // Shared across fibers and threads.
  fn send_sync<T: Send + Sync + Clone + 'static>(_: &T) {}
  send_sync(&catalog);
  let clone = catalog.clone();
  let text = std::thread::spawn(move || clone.format(&loc("ar"), "hello", &Args::new()))
    .join()
    .expect("thread");
  assert!(text.starts_with("مرحبا"));
}
