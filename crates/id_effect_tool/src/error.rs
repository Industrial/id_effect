//! [`NewError`]: why `id-effect new` refused or failed.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Why a project could not be generated.
#[derive(Debug)]
#[non_exhaustive]
pub enum NewError {
  /// The project name is not a valid package name.
  InvalidName {
    /// The rejected name.
    name: String,
    /// What is wrong with it.
    reason: &'static str,
  },
  /// An option the chosen template does not support (e.g. `--db pg` for a `cli`).
  Unsupported {
    /// The option, as on the command line.
    option: &'static str,
    /// The template it was given with.
    template: &'static str,
  },
  /// The destination exists and is not an empty directory; nothing was written.
  Exists(PathBuf),
  /// `--local-workspace` does not point at an id_effect checkout (no `crates/id_effect_kit`).
  NotAWorkspace(PathBuf),
  /// A template is malformed (an unbalanced `{{#if}}` or an unknown placeholder).
  Template {
    /// The template file.
    file: String,
    /// What is wrong.
    reason: String,
  },
  /// Reading or writing `path` failed.
  Io {
    /// The path being read or written.
    path: PathBuf,
    /// The underlying error.
    source: io::Error,
  },
}

impl fmt::Display for NewError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::InvalidName { name, reason } => write!(f, "invalid project name `{name}`: {reason}"),
      Self::Unsupported { option, template } => {
        write!(f, "{option} is not supported by the `{template}` template")
      }
      Self::Exists(path) => write!(
        f,
        "destination {} already exists and is not empty; choose another --dest",
        path.display()
      ),
      Self::NotAWorkspace(path) => write!(
        f,
        "--local-workspace {} is not an id_effect checkout (no crates/id_effect_kit/Cargo.toml)",
        path.display()
      ),
      Self::Template { file, reason } => write!(f, "template {file}: {reason}"),
      Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
    }
  }
}

impl std::error::Error for NewError {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Io { source, .. } => Some(source),
      _ => None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::error::Error as _;

  #[test]
  fn display_names_the_problem() {
    let cases = [
      (
        NewError::InvalidName {
          name: "9x".into(),
          reason: "must start with a letter",
        },
        "invalid project name `9x`: must start with a letter",
      ),
      (
        NewError::Unsupported {
          option: "--db pg",
          template: "cli",
        },
        "--db pg is not supported by the `cli` template",
      ),
      (
        NewError::Template {
          file: "service/x".into(),
          reason: "unclosed".into(),
        },
        "template service/x: unclosed",
      ),
    ];
    for (err, want) in cases {
      assert_eq!(err.to_string(), want);
      assert!(err.source().is_none());
    }
  }

  #[test]
  fn path_errors_show_the_path() {
    let exists = NewError::Exists(PathBuf::from("/tmp/app"));
    assert!(exists.to_string().contains("/tmp/app already exists"));
    let ws = NewError::NotAWorkspace(PathBuf::from("/nope"));
    assert!(
      ws.to_string()
        .contains("/nope is not an id_effect checkout")
    );
  }

  #[test]
  fn io_errors_keep_their_source() {
    let err = NewError::Io {
      path: PathBuf::from("/x/Cargo.toml"),
      source: io::Error::other("disk full"),
    };
    assert_eq!(err.to_string(), "/x/Cargo.toml: disk full");
    assert_eq!(
      err.source().map(ToString::to_string).as_deref(),
      Some("disk full")
    );
  }
}
