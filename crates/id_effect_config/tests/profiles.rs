//! Per-environment profiles: `config/base.toml` → `config/<profile>.toml` → environment
//! (sec.config #6, dx.repl #7).
#![cfg(all(feature = "toml", feature = "env"))]

use id_effect_config::{Config, ConfigError, PROFILE_ENV_VAR, Profiles, Secret};

/// Lay out `<root>/config/{base,dev,test,prod}.toml` as an application would ship them.
fn app_root() -> tempfile::TempDir {
  let root = tempfile::tempdir().expect("tempdir");
  let config = root.path().join("config");
  std::fs::create_dir(&config).expect("config dir");
  let files = [
    (
      "base.toml",
      "name = \"ledger\"\nport = 3000\nlog = \"info\"\n[db]\nhost = \"localhost\"\npassword = \"dev-only\"\n",
    ),
    ("dev.toml", "log = \"debug\"\n"),
    ("test.toml", "port = 0\n[db]\nhost = \"db.test\"\n"),
    ("prod.toml", "port = 8080\n[db]\nhost = \"db.prod\"\n"),
  ];
  for (name, body) in files {
    std::fs::write(config.join(name), body).expect("write");
  }
  root
}

#[test]
fn profile_overlay_precedence() {
  let root = app_root();
  let profiles = Profiles::new()
    .dir(root.path().join("config"))
    .env_var("IDCFG_IT_PROFILE")
    .env_prefix("IDCFG_IT_APP_");

  temp_env::with_vars(
    [
      ("IDCFG_IT_PROFILE", Some("prod")),
      ("IDCFG_IT_APP_DB__PASSWORD", Some("from-env")),
    ],
    || {
      let cfg = profiles.load().expect("load prod");
      assert_eq!(cfg.profile().as_str(), "prod");
      let p = cfg.provider();
      // base only
      assert_eq!(Config::string("name").load(&p).unwrap(), "ledger");
      assert_eq!(Config::string("log").load(&p).unwrap(), "info");
      // profile > base
      assert_eq!(Config::integer("port").load(&p).unwrap(), 8080);
      assert_eq!(Config::string("db.host").load(&p).unwrap(), "db.prod");
      // env > profile > base, and it stays redacted as a Secret
      let pw: Secret<String> = Config::string("db.password").secret().load(&p).unwrap();
      assert_eq!(pw.expose(), "from-env");
      assert_eq!(format!("{pw:?}"), "<redacted>");
    },
  );

  // The flag beats the environment variable; no env override leaves the files in charge.
  temp_env::with_var("IDCFG_IT_PROFILE", Some("prod"), || {
    let test = profiles.select(["--profile=test"], Some("prod")).unwrap();
    let p = profiles.load_for(test).unwrap().provider();
    assert_eq!(Config::integer("port").load(&p).unwrap(), 0);
    assert_eq!(Config::string("db.host").load(&p).unwrap(), "db.test");
    assert_eq!(Config::string("db.password").load(&p).unwrap(), "dev-only");
  });

  // No selection at all: the `dev` default.
  temp_env::with_var("IDCFG_IT_PROFILE", None::<&str>, || {
    let cfg = profiles.load().unwrap();
    assert_eq!(cfg.profile().as_str(), "dev");
    assert_eq!(
      Config::string("log").load(&cfg.provider()).unwrap(),
      "debug"
    );
  });
}

#[test]
fn unknown_profile_is_a_typed_error() {
  let root = app_root();
  let profiles = Profiles::new().dir(root.path().join("config"));
  temp_env::with_var(PROFILE_ENV_VAR, Some("staging"), || {
    let err = profiles.load().unwrap_err();
    assert!(matches!(
      &err,
      ConfigError::UnknownProfile { profile, known } if profile == "staging" && known == &["dev", "test", "prod"]
    ));
    assert_eq!(
      err.to_string(),
      "unknown configuration profile \"staging\" (known: dev, test, prod)"
    );
  });
}
