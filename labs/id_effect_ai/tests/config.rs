use id_effect_ai::AiConfig;

#[test]
fn default_config_has_vendor_urls() {
  let cfg = AiConfig::default();
  assert!(cfg.openai_base_url.contains("openai"));
  assert!(cfg.anthropic_base_url.contains("anthropic"));
  assert!(cfg.cursor_base_url.contains("cursor"));
}
