//! `id-effect rpc`: tools for `id_effect_rpc` service contracts (ADR 0014).
//!
//! ```text
//! id-effect rpc gen ts --contract rpc.contract.json --out web/src/rpc
//! ```
//!
//! `gen ts` writes a typed TypeScript client ([`gen_ts`]): `client.ts`, `types.ts` and
//! `runtime.ts`, plain TypeScript with no runtime dependency. Exit codes: `0` written, `1` the
//! contract could not be read or turned into TypeScript, or a file could not be written.

pub mod gen_ts;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

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
  }
}
