//! Writing a rendered project to disk ([`new_project`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::NewError;
use crate::render::{Options, render};

/// What [`new_project`] wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
  /// The project directory.
  pub root: PathBuf,
  /// The files written, relative to `root`, in template order.
  pub files: Vec<String>,
}

/// Renders `options` and writes the project into `dest` (created when missing).
///
/// Everything is checked before the first write: an invalid option, a bad
/// `--local-workspace` or a `dest` that is not a missing or empty directory writes nothing.
///
/// # Errors
///
/// The [`render`] errors, [`NewError::Exists`] for a non-empty `dest`, [`NewError::Io`] when a
/// directory or file cannot be written.
pub fn new_project(options: &Options, dest: &Path) -> Result<Report, NewError> {
  let files = render(options)?;
  check_destination(dest)?;
  let io = |path: &Path| {
    let path = path.to_path_buf();
    move |source: io::Error| NewError::Io { path, source }
  };
  let mut written = Vec::with_capacity(files.len());
  for file in files {
    let path = dest.join(&file.path);
    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent).map_err(io(parent))?;
    }
    fs::write(&path, &file.contents).map_err(io(&path))?;
    written.push(file.path);
  }
  Ok(Report {
    root: dest.to_path_buf(),
    files: written,
  })
}

/// `Ok` when `dest` is missing or an empty directory.
fn check_destination(dest: &Path) -> Result<(), NewError> {
  match fs::read_dir(dest) {
    Ok(mut entries) => match entries.next() {
      None => Ok(()),
      Some(_) => Err(NewError::Exists(dest.to_path_buf())),
    },
    Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
    Err(_) if dest.exists() => Err(NewError::Exists(dest.to_path_buf())),
    Err(source) => Err(NewError::Io {
      path: dest.to_path_buf(),
      source,
    }),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::template::Template;

  #[test]
  fn writes_every_rendered_file_into_a_new_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("nested/app");
    let options = Options::new("app", Template::Cli);
    let report = new_project(&options, &dest).expect("new");
    assert_eq!(report.root, dest);
    let rendered = render(&options).unwrap();
    assert_eq!(report.files.len(), rendered.len());
    for file in rendered {
      let on_disk = fs::read_to_string(dest.join(&file.path)).expect(&file.path);
      assert_eq!(on_disk, file.contents, "{}", file.path);
    }
    assert!(dest.join(".github/workflows/ci.yml").is_file());
  }

  #[test]
  fn an_existing_empty_directory_is_fine() {
    let tmp = tempfile::tempdir().unwrap();
    let report = new_project(&Options::new("app", Template::Worker), tmp.path()).expect("new");
    assert!(report.files.iter().any(|f| f == "Cargo.toml"));
  }

  #[test]
  fn refuses_a_non_empty_directory_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("keep.txt"), "mine").unwrap();
    let err = new_project(&Options::new("app", Template::Service), tmp.path()).unwrap_err();
    assert!(
      matches!(&err, NewError::Exists(p) if p == tmp.path()),
      "{err}"
    );
    let names: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
    assert_eq!(names.len(), 1);
  }

  #[test]
  fn refuses_a_file_as_destination() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("app");
    fs::write(&file, "").unwrap();
    let err = new_project(&Options::new("app", Template::Cli), &file).unwrap_err();
    assert!(matches!(err, NewError::Exists(_)), "{err}");
  }

  #[test]
  fn invalid_options_write_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("app");
    let options = Options::new("app", Template::Cli).otel(true);
    assert!(matches!(
      new_project(&options, &dest),
      Err(NewError::Unsupported { .. })
    ));
    assert!(!dest.exists());
  }

  #[test]
  fn a_destination_under_a_file_is_an_io_error() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("plain");
    fs::write(&file, "").unwrap();
    let err = new_project(&Options::new("app", Template::Cli), &file.join("app")).unwrap_err();
    assert!(matches!(err, NewError::Io { .. }), "{err}");
  }
}
