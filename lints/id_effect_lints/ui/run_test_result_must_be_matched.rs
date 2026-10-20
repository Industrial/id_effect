// I-02: `exit.into_result().unwrap()`.
#![allow(dead_code)]

enum Exit {
  Success(u8),
  Failure(String),
}

impl Exit {
  fn into_result(self) -> Result<u8, String> {
    match self {
      Exit::Success(v) => Ok(v),
      Exit::Failure(e) => Err(e),
    }
  }
}

fn check(exit: Exit) -> u8 {
  exit.into_result().unwrap()
}

fn matched(exit: Exit) -> bool {
  matches!(exit, Exit::Success(_))
}

fn main() {}
