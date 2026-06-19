//! Authentication trait surfaces (session store, OAuth client).

pub mod oauth;
pub mod session;

pub use oauth::{MemoryOAuthClient, OAuthClient, OAuthError, OAuthTokens, OAuthUserInfo};
pub use session::{MemorySessionStore, SessionData, SessionError, SessionStore};
