//! p-config-unify: `Config<T>` as the sole API — `Config::all` accumulates errors,
//! `Config::layer()` provides the loaded value as a capability, providers are capabilities.

use std::collections::HashMap;

use id_effect::Exit;
use id_effect::next::{Effect, HCons, HNil, Layer, need};
use id_effect_config::{
  Config, ConfigEnv, ConfigError, ConfigProvider, ConfigProviderService, EnvConfigProvider,
  FigmentConfigProvider, MapConfigProvider, OrElseConfigProvider, ProviderOptions, config_env,
};

fn map(pairs: &[(&str, &str)]) -> MapConfigProvider {
  MapConfigProvider::from_pairs(pairs.iter().copied())
}

fn missing_paths(err: &ConfigError) -> Vec<String> {
  err
    .errors()
    .iter()
    .filter_map(|e| match e {
      ConfigError::Missing { path } => Some(path.clone()),
      _ => None,
    })
    .collect()
}

/// Run `eff` and return its typed failure (panics on success or a defect).
fn failure<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> E {
  match eff.block_on() {
    Exit::Success(_) => panic!("expected a failure"),
    Exit::Failure(cause) => cause.fail_option().expect("a typed failure"),
  }
}

// ── `Config::all` accumulates ────────────────────────────────────────────────

#[test]
fn all_tuple_reports_every_missing_and_invalid_key() {
  let p = map(&[("PORT", "not-a-number"), ("NAME", "svc")]);
  let err = Config::all((
    Config::string("HOST"),
    Config::integer("PORT"),
    Config::string("NAME"),
    Config::boolean("DEBUG"),
  ))
  .load(&p)
  .unwrap_err();
  let ConfigError::And(errors) = &err else {
    panic!("expected And, got {err:?}")
  };
  assert_eq!(errors.len(), 3);
  assert!(matches!(&errors[0], ConfigError::Missing { path } if path == "HOST"));
  assert!(matches!(&errors[1], ConfigError::Invalid { path, .. } if path == "PORT"));
  assert!(matches!(&errors[2], ConfigError::Missing { path } if path == "DEBUG"));
  assert!(!err.is_missing_only());
  let shown = err.to_string();
  assert!(shown.starts_with("3 configuration errors: "), "{shown}");
  assert!(shown.contains("\"HOST\"") && shown.contains("\"PORT\"") && shown.contains("\"DEBUG\""));
}

#[test]
fn all_single_failure_is_not_wrapped() {
  let err = Config::all((Config::string("A"), Config::string("B")))
    .load(&map(&[("A", "x")]))
    .unwrap_err();
  assert!(matches!(err, ConfigError::Missing { ref path } if path == "B"));
  assert_eq!(err.errors().len(), 1);
}

#[test]
fn all_vec_and_array_accumulate() {
  let p = map(&[("K1", "1")]);
  let keys = || ["K0", "K1", "K2"].map(Config::integer);
  let err = Config::all(keys().to_vec()).load(&p).unwrap_err();
  assert_eq!(missing_paths(&err), ["K0", "K2"]);
  let err = Config::all(keys()).load(&p).unwrap_err();
  assert_eq!(missing_paths(&err), ["K0", "K2"]);

  let full = map(&[("K0", "0"), ("K1", "1"), ("K2", "2")]);
  assert_eq!(Config::all(keys()).load(&full).unwrap(), [0, 1, 2]);
  assert_eq!(
    Config::all(keys().to_vec()).load(&full).unwrap(),
    vec![0, 1, 2]
  );
}

#[test]
fn nested_alls_flatten_into_one_and() {
  let inner = Config::all((Config::string("A"), Config::string("B")));
  let outer = Config::all((inner, Config::string("C")));
  let err = outer.load(&map(&[])).unwrap_err();
  assert_eq!(missing_paths(&err), ["A", "B", "C"]);
}

#[test]
fn zip_and_zip_with_accumulate() {
  let err = Config::string("HOST")
    .zip_with(Config::integer("PORT"), |h, p| format!("{h}:{p}"))
    .load(&map(&[]))
    .unwrap_err();
  assert_eq!(missing_paths(&err), ["HOST", "PORT"]);
  let pair = Config::string("HOST")
    .zip(Config::integer("PORT"))
    .load(&map(&[("HOST", "h"), ("PORT", "1")]))
    .unwrap();
  assert_eq!(pair, ("h".to_string(), 1));
}

#[test]
fn all_accumulates_through_run() {
  let program = Config::all((Config::string("HOST"), Config::integer("PORT")))
    .run::<(String, i64), ConfigError, ConfigEnv, _>()
    .provide_all(config_env(map(&[])));
  assert_eq!(missing_paths(&failure(program)), ["HOST", "PORT"]);
}

// ── Fallbacks see composite errors (Effect `isMissingDataOnly`) ──────────────

#[test]
fn with_default_applies_when_every_failure_is_missing() {
  let pair = Config::all((Config::string("A"), Config::string("B")))
    .with_default(("a".into(), "b".into()))
    .load(&map(&[]))
    .unwrap();
  assert_eq!(pair, ("a".to_string(), "b".to_string()));
}

#[test]
fn with_default_does_not_hide_an_invalid_key() {
  let err = Config::all((Config::string("A"), Config::integer("N")))
    .with_default(("a".into(), 0))
    .load(&map(&[("N", "x")]))
    .unwrap_err();
  assert!(
    err
      .errors()
      .iter()
      .any(|e| matches!(e, ConfigError::Invalid { .. }))
  );
}

#[test]
fn or_else_falls_back_on_composite_missing() {
  let primary = Config::all((Config::string("A"), Config::string("B")));
  let fallback = Config::all((Config::string("X"), Config::string("Y")));
  let v = primary
    .or_else(fallback)
    .load(&map(&[("X", "x"), ("Y", "y")]))
    .unwrap();
  assert_eq!(v, ("x".to_string(), "y".to_string()));
}

// ── `Config::layer` ──────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
struct Port(i64);

#[derive(Debug, PartialEq)]
struct Host(String);

fn port_layer() -> Layer<HCons<Port, HNil>, ConfigError, ConfigEnv> {
  Config::integer("PORT").map(Port).layer()
}

fn host_layer() -> Layer<HCons<Host, HNil>, ConfigError, ConfigEnv> {
  Config::string("HOST").map(Host).layer()
}

#[test]
fn layer_provides_the_loaded_value_as_a_capability() {
  let app = map(&[("PORT", "8080")]).layer().provide_to(port_layer());
  let program: Effect<i64, ConfigError, HCons<Port, HNil>> = need::<Port>().map(|p| p.0);
  assert!(matches!(
    program.provide_layer(app).block_on(),
    Exit::Success(8080)
  ));
}

#[test]
fn layer_build_fails_with_the_accumulated_error() {
  let server = Config::all((Config::string("HOST"), Config::integer("PORT")))
    .layer::<ConfigError>()
    .build()
    .provide_all(config_env(map(&[])));
  assert_eq!(missing_paths(&failure(server)), ["HOST", "PORT"]);
}

#[test]
fn layer_error_converts_into_the_graph_error() {
  #[derive(Debug)]
  struct AppError(ConfigError);
  impl From<ConfigError> for AppError {
    fn from(e: ConfigError) -> Self {
      AppError(e)
    }
  }
  let built = Config::integer("PORT")
    .map(Port)
    .layer::<AppError>()
    .build()
    .provide_all(config_env(map(&[("PORT", "x")])));
  assert!(matches!(
    failure(built),
    AppError(ConfigError::Invalid { .. })
  ));
}

#[test]
fn config_layers_merge_over_one_provider() {
  let app = map(&[("HOST", "::1"), ("PORT", "443")])
    .layer()
    .provide_to(host_layer().merge(port_layer()));
  let program: Effect<String, ConfigError, HCons<Host, HCons<Port, HNil>>> =
    need::<Host>().flat_map(|h| need::<Port>().map(move |p| format!("{}:{}", h.0, p.0)));
  assert!(matches!(
    program.provide_layer(app).block_on(),
    Exit::Success(s) if s == "::1:443"
  ));
}

#[test]
fn layer_reloads_on_each_build() {
  let layer = Config::integer("PORT").map(Port).layer::<ConfigError>();
  for (raw, want) in [("1", 1), ("2", 2)] {
    let built = layer.build().provide_all(config_env(map(&[("PORT", raw)])));
    let Exit::Success(env) = built.block_on() else {
      panic!("build failed")
    };
    assert_eq!(env.0.0, want);
  }
}

// ── Providers are capabilities ───────────────────────────────────────────────

fn read_k(layer: Layer<ConfigEnv, ConfigError>) -> String {
  let program = Config::string("K")
    .run::<String, ConfigError, ConfigEnv, _>()
    .provide_layer(layer);
  match program.block_on() {
    Exit::Success(v) => v,
    Exit::Failure(cause) => panic!("read failed: {cause:?}"),
  }
}

#[test]
fn every_provider_is_installable_as_the_capability() {
  assert_eq!(read_k(map(&[("K", "map")]).layer()), "map");
  assert_eq!(
    read_k(OrElseConfigProvider::new(map(&[]), map(&[("K", "fallback")])).layer()),
    "fallback"
  );
  assert_eq!(
    read_k(map(&[("NS_K", "scoped")]).within("NS").layer()),
    "scoped"
  );
  let figment =
    ::figment::Figment::from(::figment::providers::Serialized::defaults(HashMap::from([
      ("K", "figment"),
    ])));
  assert_eq!(
    read_k(FigmentConfigProvider::new(figment).layer()),
    "figment"
  );
  assert_eq!(
    read_k(ConfigProviderService::new(map(&[("K", "service")])).layer()),
    "service"
  );
  // The env provider reads the process environment; only check that it is installable.
  let env_layer: Layer<ConfigEnv, ConfigError> =
    EnvConfigProvider::new(ProviderOptions::default()).layer();
  let _ = env_layer;
}

#[test]
fn the_installed_service_is_a_provider() {
  let svc = ConfigProviderService::new(map(&[("A_B", "v")]));
  assert_eq!(Config::string("B").nested("A").load(&svc).unwrap(), "v");
}
