//! `id-effect rpc`: tools for `id_effect_rpc` service contracts (ADR 0014).
//!
//! ```text
//! id-effect rpc gen ts --contract rpc.contract.json --out web/src/rpc
//! ```
//!
//! `gen ts` writes a typed TypeScript client ([`gen_ts`]): `client.ts`, `types.ts` and
//! `runtime.ts`, plain TypeScript with no runtime dependency. Exit codes: `0` written, `1` the
//! contract could not be read or turned into TypeScript, or a file could not be written.
//!
//! ```text
//! id-effect rpc check --contract rpc.contract.json --against v0.11.0:rpc.contract.json
//! ```
//!
//! `check` classifies every change against the previous contract and fails on a breaking change
//! within a published version ([`check`]); `--generated <dir>` also fails when the checked-in
//! TypeScript client is stale. `--allow-retired Service:N` (repeatable, [`RetiredVersion`]) lets
//! the removal of that service version pass, reported as `retired`; naming a version that was not
//! removed is a usage error. Exit codes: `0` passes, `1` a breaking change or a stale client, `2` a
//! usage error or an unreadable contract.

pub mod check;
pub mod gen_ts;

use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use clap::{Args, Subcommand};

/// Arguments of `id-effect rpc`.
#[derive(Debug, Args)]
pub struct RpcArgs {
  /// What to do.
  #[command(subcommand)]
  pub command: RpcCommand,
}

/// The `id-effect rpc` subcommands. Non-exhaustive: `check` and others are added in minor
/// releases.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum RpcCommand {
  /// Generate a client from a service contract (`gen ts`: TypeScript).
  Gen(GenArgs),
  /// Compare a contract with the previous one (a file or `<git-ref>:<path>`) and fail on a
  /// breaking change within a published version; `--generated` also checks the TypeScript client.
  Check(check::CheckArgs),
}

/// Arguments of `id-effect rpc gen`.
#[derive(Debug, Args)]
pub struct GenArgs {
  /// The client language.
  #[command(subcommand)]
  pub target: GenTarget,
}

/// The languages `id-effect rpc gen` writes clients in.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum GenTarget {
  /// A typed TypeScript client (client.ts, types.ts, runtime.ts): batching per microtask,
  /// WebTransport, then WebSocket, then SSE + POST, reconnect and resume, AbortSignal.
  Ts(GenTsArgs),
}

/// Arguments of `id-effect rpc gen ts`.
#[derive(Debug, Args)]
pub struct GenTsArgs {
  /// The service contract (`rpc.contract.json`, from `XService::contract()`).
  #[arg(long, value_name = "FILE")]
  pub contract: PathBuf,
  /// The directory to write client.ts, types.ts and runtime.ts into (created when missing).
  #[arg(long, value_name = "DIR")]
  pub out: PathBuf,
}

/// A service version whose removal is intended: a value of `id-effect rpc check
/// --allow-retired Service:N`.
///
/// Parsed from `Service:N` (the last `:` separates the version, a decimal `u32`). A retired version
/// only excuses its own `version no longer served` change ([`check::retire`]); every other breaking
/// change still fails the check.
///
/// ```rust
/// use id_effect_tool::rpc::RetiredVersion;
///
/// let retired: RetiredVersion = "Greeter:1".parse().expect("Service:N");
/// assert_eq!(retired, RetiredVersion::new("Greeter", 1));
/// assert_eq!(retired.to_string(), "Greeter:1");
/// assert!("Greeter".parse::<RetiredVersion>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RetiredVersion {
  /// The service name, as in the contract.
  pub service: String,
  /// The removed version.
  pub version: u32,
}

impl RetiredVersion {
  /// The retired `version` of `service`.
  pub fn new(service: impl Into<String>, version: u32) -> Self {
    Self {
      service: service.into(),
      version,
    }
  }
}

impl FromStr for RetiredVersion {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let expected = || format!("`{s}`: expected `Service:N`, e.g. `Greeter:1`");
    let (service, version) = s.rsplit_once(':').ok_or_else(expected)?;
    if service.is_empty() {
      return Err(expected());
    }
    let version = version
      .parse::<u32>()
      .map_err(|e| format!("`{s}`: version `{version}`: {e}"))?;
    Ok(Self::new(service, version))
  }
}

impl fmt::Display for RetiredVersion {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}:{}", self.service, self.version)
  }
}

/// Runs `id-effect rpc`, writing to `out` and `err`.
pub fn run(args: &RpcArgs, out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
  match &args.command {
    RpcCommand::Gen(GenArgs {
      target: GenTarget::Ts(args),
    }) => match gen_ts::generate_to(&args.contract, &args.out) {
      Ok(paths) => {
        for path in paths {
          let _ = writeln!(out, "wrote {}", path.display());
        }
        ExitCode::SUCCESS
      }
      Err(e) => {
        let _ = writeln!(err, "error: {e}");
        ExitCode::FAILURE
      }
    },
    RpcCommand::Check(args) => check::run(args, out, err),
  }
}
