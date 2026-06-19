//! Create a Cursor Cloud Agent and poll until idle (requires `CURSOR_API_KEY`).

use id_effect::succeed;
use id_effect_ai::{
  AiConfig, AiError, CreateAgentRequest, CursorAgentsClient, HttpCursorAgentsClient,
};
use id_effect_cli::{RunMainConfig, run_main};
use id_effect_platform::http::ReqwestHttpClient;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

fn main() -> ExitCode {
  let cfg = AiConfig::from_env();
  let client = Arc::new(ReqwestHttpClient::default_client());
  let agents = match HttpCursorAgentsClient::new(client, cfg) {
    Ok(a) => a,
    Err(e) => {
      eprintln!("{e}");
      return ExitCode::FAILURE;
    }
  };
  let prompt = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "List project README sections.".into());
  let effect = agents
    .create_agent(CreateAgentRequest {
      prompt_text: prompt,
      model_id: std::env::var("CURSOR_MODEL").ok(),
      repos: vec![],
    })
    .flat_map(move |(agent, run)| {
      let agents = agents.clone();
      let agent_id = agent.id.clone();
      agents
        .wait_until_idle(&agent.id, &run.id, Duration::from_secs(120))
        .flat_map(move |done| {
          println!("agent {} finished: {:?}", agent_id, done.status);
          succeed::<(), AiError, ()>(())
        })
    });
  run_main(effect, (), RunMainConfig::minimal())
}
