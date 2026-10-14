//! CLI: ask once via OpenAI or Anthropic (`--vendor`).

use clap::{Parser, ValueEnum};
use id_effect::{Env, build_env, succeed};
use id_effect_ai::{
  AiConfig, AiError, ChatMessage, ChatRequest, ChatRole, complete,
  provide_anthropic_language_model, provide_openai_language_model,
};
use id_effect_cli::{RunMainConfig, run_main};
use id_effect_platform::http::{ReqwestHttpClient, provide_reqwest_http_client};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "ask_once")]
struct Args {
  #[arg(long, value_enum, default_value_t = Vendor::Openai)]
  vendor: Vendor,
  #[arg(long)]
  model: Option<String>,
  #[arg(long, default_value = "Hello")]
  prompt: String,
}

#[derive(Clone, Copy, ValueEnum)]
enum Vendor {
  Openai,
  Anthropic,
}

fn main() -> ExitCode {
  let args = Args::parse();
  let cfg = AiConfig::from_env();
  let client = Arc::new(ReqwestHttpClient::default_client());
  let provider = match args.vendor {
    Vendor::Openai => provide_openai_language_model(client, cfg).expect("openai"),
    Vendor::Anthropic => provide_anthropic_language_model(client, cfg).expect("anthropic"),
  };
  let env = build_env([provide_reqwest_http_client(), provider]).expect("env");
  let model = match args.vendor {
    Vendor::Openai => args.model.unwrap_or_else(|| "gpt-4o-mini".into()),
    Vendor::Anthropic => args
      .model
      .unwrap_or_else(|| "claude-sonnet-4-20250514".into()),
  };
  let req = ChatRequest {
    model,
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: args.prompt,
    }],
  };
  let effect: id_effect::Effect<(), AiError, Env> = complete(req).flat_map(|resp| {
    println!("{}", resp.content);
    succeed::<(), AiError, Env>(())
  });
  run_main(effect, env, RunMainConfig::minimal())
}
