//! Config profile TOML merge (id_effect_config `Profiles`): the input is split at `\n---\n`
//! into `base.toml` and `prod.toml`, written to a per-process directory, merged with
//! `Profiles::load_for(prod)` and read back as a whole document and through `Config`
//! descriptors.
//!
//! Checks besides "no panic": a document that merges also extracts.
#![no_main]

use std::path::PathBuf;
use std::sync::LazyLock;

use id_effect_config::{Config, Profiles};
use id_effect_fuzz::split_two;
use libfuzzer_sys::fuzz_target;

static DIR: LazyLock<PathBuf> = LazyLock::new(|| {
  let dir = std::env::temp_dir().join(format!("id_effect_fuzz_profile_{}", std::process::id()));
  std::fs::create_dir_all(&dir).expect("create the profile directory");
  dir
});

fuzz_target!(|data: &[u8]| {
  let (base, prod) = split_two(data);
  std::fs::write(DIR.join("base.toml"), base).expect("write base.toml");
  std::fs::write(DIR.join("prod.toml"), prod).expect("write prod.toml");
  // A prefix no environment sets, so only the two files merge.
  let profiles = Profiles::new()
    .dir(DIR.as_path())
    .env_prefix("ID_EFFECT_FUZZ_UNSET_");
  let prod = profiles.profile("prod").expect("prod is declared");
  let Ok(loaded) = profiles.load_for(prod) else {
    return;
  };
  loaded
    .extract::<serde_json::Value>()
    .expect("a merged document extracts");
  let provider = loaded.provider();
  let _ = Config::string("host").load(&provider);
  let _ = Config::integer("port").load(&provider);
  let _ = Config::string("db.url").load(&provider);
});
