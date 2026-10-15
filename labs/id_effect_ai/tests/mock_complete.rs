use id_effect_ai::{ChatMessage, ChatRequest, ChatRole, LanguageModel, MockLanguageModel};

#[test]
fn mock_complete_echoes_user() {
  let model = MockLanguageModel::echo();
  let req = ChatRequest {
    model: "mock".into(),
    messages: vec![ChatMessage {
      role: ChatRole::User,
      content: "hello".into(),
    }],
  };
  let resp = model.complete(req).block_on().into_result().expect("run");
  assert_eq!(resp.content, "echo:hello");
}
