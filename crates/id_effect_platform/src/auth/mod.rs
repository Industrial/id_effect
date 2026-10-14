//! Authentication capability traits (session store, OAuth client).

pub mod oauth;
pub mod session;

#[cfg(any(test, feature = "test-util"))]
pub use oauth::MemoryOAuthClient;
pub use oauth::{OAuthClient, OAuthError, OAuthTokens, OAuthUserInfo};
pub use session::{MemorySessionStore, SessionData, SessionError, SessionStore};
