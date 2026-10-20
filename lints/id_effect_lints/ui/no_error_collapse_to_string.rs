// F-02: an error collapsed to a `String`.
#![allow(dead_code)]

fn to_string(e: std::io::Error) -> Result<(), String> {
  Err(e.to_string())
}

fn formatted(e: std::io::Error) -> Result<(), String> {
  Err(format!("io: {e}"))
}

#[derive(Debug)]
enum AppError {
  Io(std::io::Error),
}

fn typed(e: std::io::Error) -> Result<(), AppError> {
  Err(AppError::Io(e))
}

fn main() {}
