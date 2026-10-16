//! Ex 010 — what `id-effect new` generates: every template rendered in memory, then one written
//! to a temporary directory (and a second write into it refused).
//!
//! Run: `cargo run -p id_effect_tool --example 010_render`

use id_effect_tool::{Db, NewError, Options, Template, new_project, render};

fn main() -> Result<(), NewError> {
  for template in Template::ALL {
    let files = render(&Options::new("demo", template))?;
    println!("{template}: {} files", files.len());
    for file in &files {
      println!("  {:<28} {:>5} bytes", file.path, file.contents.len());
    }
  }

  let with_pg = render(
    &Options::new("demo", Template::Service)
      .db(Db::Pg)
      .otel(true),
  )?;
  println!("service --db pg --otel: {} files", with_pg.len());

  let dest = std::env::temp_dir().join(format!("id-effect-010-{}", std::process::id()));
  let report = new_project(&Options::new("demo", Template::Cli), &dest)?;
  println!(
    "wrote {} files to {}",
    report.files.len(),
    report.root.display()
  );
  match new_project(&Options::new("demo", Template::Cli), &dest) {
    Err(e @ NewError::Exists(_)) => println!("second run refused: {e}"),
    other => panic!("expected a refusal, got {other:?}"),
  }
  std::fs::remove_dir_all(&dest).map_err(|source| NewError::Io { path: dest, source })
}
