use id_effect::run_blocking;
use id_effect_host::auth::{MemoryOAuthClient, OAuthClient, OAuthError, OAuthUserInfo};

#[test]
fn memory_client_authorization_url_contains_state() {
  let client = MemoryOAuthClient::new("https://idp.test");
  let url = client.authorization_url("state123");
  assert!(url.contains("state123"));
}

#[test]
fn memory_client_exchange_registered_code() {
  let client = MemoryOAuthClient::new("https://idp.test");
  client.register_code(
    "code-1",
    OAuthUserInfo {
      sub: "u1".into(),
      email: Some("a@b.c".into()),
    },
  );
  let (tokens, user) = run_blocking(client.exchange_code("code-1"), ()).expect("exchange");
  assert!(!tokens.access_token.is_empty());
  assert_eq!(user.sub, "u1");
}

#[test]
fn memory_client_rejects_unknown_code() {
  let client = MemoryOAuthClient::new("https://idp.test");
  let err = run_blocking(client.exchange_code("nope"), ()).unwrap_err();
  assert!(matches!(err, OAuthError::InvalidCode));
}
