//! Sign-in: organisations and their users, password login into a PostgreSQL session, a TOTP
//! second factor, API tokens (JWT bearer) and their revocation.
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `POST /orgs` | anyone (rate-limited) | a new organisation and its owner, signed in |
//! | `POST /auth/login` | anyone (rate-limited) | password (and TOTP code once enrolled) → session cookie |
//! | `POST /auth/logout` | signed in | ends the session |
//! | `GET /me` | signed in | the caller |
//! | `POST /auth/totp/enrol`, `/confirm` | a password user | enrol a TOTP authenticator |
//! | `POST /users` | owner after TOTP (`users.manage`) | a user of the organisation |
//! | `POST /auth/tokens`, `GET /auth/tokens` | `tokens.issue` / signed in | API tokens of the caller |
//! | `DELETE /auth/tokens/{id}` | the token's owner (`token.revoke`) | revoke it |
//!
//! A session's principal is rebuilt on every request from its attributes (`roles`, `tenant`,
//! and the [`crate::policy::MFA`] permission after a TOTP login). An API token carries the
//! roles and organisation of whoever issued it, never the MFA permission.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use id_effect::schema::{self, HasSchema, ParseError, Schema, length, one_of, pattern};
use id_effect::{Effect, Exit, HNil, fail, succeed};
use id_effect_kit::auth::{
  AuthError, AuthPrincipal, AuthScheme, PasswordHash, Session, SessionId, TotpError, TotpSecret,
  TotpStepStore, TotpVerification, Verification,
};
use id_effect_kit::authz::{Policies, Principal};
use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::axum::{ClientIp, RequestCaps, run_with_env};
use id_effect_kit::sql::query_builder::Dialect;
use id_effect_kit::sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_kit::sql::{
  Entity, FromRow, Repository, SqlClient, SqlError, params, with_transaction,
};
use serde::Serialize;

use crate::app::{App, AppEnv, Caller, as_caller, guarded};
use crate::error::ApiError;
use crate::limits::{LoginLimit, TenantHint};
use crate::policy::{MFA, OWNER, ROLES};
use crate::{db, signup};

/// An API token: who issued it, for which organisation. Revoking it is a soft delete.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(
  crate = id_effect_kit::sql,
  table = "api_tokens",
  tenant = "org_id",
  soft_delete = "revoked_at"
)]
pub struct ApiToken {
  /// The token's `jti` claim.
  #[entity(id)]
  pub id: String,
  /// The organisation.
  pub org_id: String,
  /// Who issued it (only they may revoke it).
  pub owner_id: String,
  /// A label.
  pub name: String,
}

/// A password user, as stored.
#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct UserRow {
  /// None for a registered user that is in no organisation yet.
  org_id: Option<String>,
  username: String,
  password_hash: String,
  role: String,
  totp_secret: Option<String>,
  /// A registered user's address (`crate::signup`).
  email: Option<String>,
  email_verified: bool,
}

const USER_COLUMNS: &str =
  "org_id, username, password_hash, role, totp_secret, email, email_verified";

fn find_user(db: &Arc<dyn SqlClient>, username: &str) -> Effect<Option<UserRow>, SqlError> {
  let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE username = $1");
  db::query_all::<UserRow>(db, &sql, params![username]).map(|rows| rows.into_iter().next())
}

/// TOTP replay protection in the `users` table: each user's last consumed step, raised by a
/// compare-and-set, so a code is accepted once across every instance.
#[derive(Clone)]
pub struct UserTotpSteps(Arc<dyn SqlClient>);

impl UserTotpSteps {
  /// Steps kept in `db`.
  pub fn new(db: Arc<dyn SqlClient>) -> Self {
    Self(db)
  }
}

impl TotpStepStore for UserTotpSteps {
  fn consume(&self, user_id: &str, step: u64) -> Effect<bool, TotpError> {
    let step = i64::try_from(step).unwrap_or(i64::MAX);
    self
      .0
      .execute(
        "UPDATE users SET totp_last_step = $2 \
         WHERE username = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)",
        &params![user_id, step],
      )
      .map(|updated| updated > 0)
      .map_err(|e| TotpError::Storage(e.to_string()))
  }
}

/// The `Cookie` header, for the session a request presents.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct CookieHeader {
  /// The raw header.
  pub cookie: Option<String>,
}

/// The caller, as `GET /me` and the sign-in routes answer it.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct Me {
  /// Username (or the identity provider's `preferred_username`).
  pub id: String,
  /// The organisation.
  pub org: String,
  /// Roles.
  pub roles: Vec<String>,
  /// Whether the session passed a second factor.
  pub mfa: bool,
}

fn username() -> Schema<String, String> {
  pattern(
    length(schema::string::<()>(), 3, 64),
    "^[a-z0-9._-]+$",
    |name| {
      name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    },
  )
}

fn password() -> Schema<String, String> {
  length(schema::string::<()>(), 12, 256)
}

/// The body of `POST /orgs`.
#[derive(Clone, Debug, PartialEq)]
pub struct Register {
  /// The organisation's id (`acme`).
  pub org: String,
  /// The owner's username.
  pub username: String,
  /// The owner's password (12+ characters).
  pub password: String,
}

impl HasSchema for Register {
  type A = Register;
  type I = (String, String, String);
  type E = ();

  fn schema() -> Schema<Register, (String, String, String)> {
    let org = pattern(
      length(schema::string::<()>(), 2, 32),
      "^[a-z0-9][a-z0-9-]{1,31}$",
      |org| {
        !org.starts_with('-')
          && org
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
      },
    );
    schema::transform(
      schema::struct3("org", org, "username", username(), "password", password()),
      |(org, username, password)| {
        Ok::<_, ParseError>(Register {
          org,
          username,
          password,
        })
      },
      |r: Register| (r.org, r.username, r.password),
    )
  }
}

/// The body of `POST /auth/login` (`totp` may be left out until the user enrolled).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct Login {
  /// Username.
  pub username: String,
  /// Password.
  pub password: String,
  /// The authenticator's code, once the user enrolled TOTP.
  pub totp: Option<String>,
}

/// The body of `POST /users`.
#[derive(Clone, Debug, PartialEq)]
pub struct NewUser {
  /// Username, unique across the service.
  pub username: String,
  /// Password (12+ characters).
  pub password: String,
  /// `owner`, `accountant` or `viewer`.
  pub role: String,
}

impl HasSchema for NewUser {
  type A = NewUser;
  type I = (String, String, String);
  type E = ();

  fn schema() -> Schema<NewUser, (String, String, String)> {
    schema::transform(
      schema::struct3(
        "username",
        username(),
        "password",
        password(),
        "role",
        one_of(schema::string::<()>(), &ROLES),
      ),
      |(username, password, role)| {
        Ok::<_, ParseError>(NewUser {
          username,
          password,
          role,
        })
      },
      |u: NewUser| (u.username, u.password, u.role),
    )
  }
}

/// A created user.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct UserCreated {
  /// Username.
  pub username: String,
  /// Role.
  pub role: String,
}

/// The body of `POST /auth/totp/confirm`.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct TotpCode {
  /// The authenticator's current code.
  pub code: String,
}

/// A TOTP enrolment: show the URI as a QR code, or the secret for typing. Shown once.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct Enrolment {
  /// The base32 secret.
  pub secret: String,
  /// The `otpauth://` URI.
  pub uri: String,
}

/// The body of `POST /auth/tokens`.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct NewToken {
  /// A label for the token.
  pub name: String,
}

/// A new API token. The `token` is shown once; the service keeps its id only.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct TokenCreated {
  /// The token id (`jti`).
  pub id: String,
  /// Its label.
  pub name: String,
  /// The bearer token.
  pub token: String,
  /// Seconds until it expires.
  pub expires_in: i64,
}

/// `GET /auth/tokens`.
#[derive(Clone, Debug, Serialize, id_effect::Schema)]
pub struct TokenList {
  /// The caller's live tokens.
  pub tokens: Vec<ApiToken>,
}

/// `{id}` of `DELETE /auth/tokens/{id}`.
pub struct TokenId;

impl HasSchema for TokenId {
  type A = String;
  type I = String;
  type E = ();

  fn schema() -> Schema<String, String> {
    length(schema::string::<()>(), 1, 64)
  }
}

/// Session attribute holding a TOTP secret between enrolment and confirmation.
const TOTP_PENDING_ATTR: &str = "totp_pending";

fn tokens_of(org: &str) -> Repository<ApiToken> {
  Repository::new(Dialect::Postgres).with_tenant(org.to_owned())
}

/// The session the request presents, if it is live.
fn presented_session(app: &App, headers: &CookieHeader) -> Option<SessionId> {
  headers
    .cookie
    .as_deref()
    .and_then(|cookie| app.sessions.session_id(cookie))
}

/// A fresh session for `user` (a new id: a presented one is dropped), carrying its roles,
/// organisation and, after a second factor, the MFA permission; answered with its cookie.
fn sign_in(
  app: &App,
  user: &UserRow,
  mfa: bool,
  presented: Option<SessionId>,
  status: StatusCode,
) -> Effect<Response, ApiError> {
  let sessions = app.sessions.clone();
  let me = Me {
    id: user.username.clone(),
    org: user.org_id.clone().unwrap_or_default(),
    roles: vec![user.role.clone()],
    mfa,
  };
  let tenant = user.org_id.clone();
  app
    .sessions
    .login(user.username.clone(), presented.as_ref())
    .flat_map(move |mut session: Session| {
      session.set_attr("roles", me.roles.join(" "));
      // A user in no organisation has no tenant: every ledger route refuses it.
      if let Some(tenant) = tenant {
        session.set_attr("tenant", tenant);
      }
      if mfa {
        session.set_attr("permissions", MFA);
      }
      let cookie = sessions.set_cookie(&session);
      sessions
        .save(session)
        .map(move |()| (status, [(header::SET_COOKIE, cookie)], Json(me)).into_response())
    })
    .map_err(ApiError::from)
}

fn register(
  env: &mut AppEnv,
  _caps: RequestCaps,
  input: Input<(), (), CookieHeader, Register>,
) -> Effect<Response, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let presented = presented_session(&app, &input.headers);
  let Register {
    org,
    username,
    password,
  } = input.body;
  app
    .hasher
    .hash(password)
    .map_err(ApiError::from)
    .flat_map({
      let app = Arc::clone(&app);
      move |hash| {
        let user = UserRow {
          org_id: Some(org.clone()),
          username,
          password_hash: hash.as_str().to_owned(),
          role: OWNER.to_owned(),
          totp_secret: None,
          email: None,
          email_verified: false,
        };
        let row = user.clone();
        with_transaction(Arc::clone(&app.db), move |tx| {
          tx.execute("INSERT INTO orgs (id) VALUES ($1)", &params![org.as_str()])
            .flat_map(move |_| {
              tx.execute(
                "INSERT INTO users (org_id, username, password_hash, role) VALUES ($1, $2, $3, $4)",
                &params![
                  org.as_str(),
                  row.username.as_str(),
                  row.password_hash.as_str(),
                  row.role.as_str()
                ],
              )
            })
        })
        .map_err(ApiError::from)
        .flat_map(move |_| sign_in(&app, &user, false, presented, StatusCode::CREATED))
      }
    })
    .narrow()
}

fn login(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), CookieHeader, Login>,
) -> Effect<Response, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let presented = presented_session(&app, &input.headers);
  let hint = caps.get::<TenantHint>().map(|hint| hint.0.clone());
  let client = caps.get::<ClientIp>().map(|ip| ip.0);
  let Login {
    username,
    password,
    totp,
  } = input.body;
  let throttle = app.throttle.clone();
  let account = username.clone();
  // Throttled before the user is looked up or a password hashed (H5).
  throttle
    .check(&username, client)
    .map_err(ApiError::from)
    .flat_map({
      let app = Arc::clone(&app);
      move |()| find_user(&app.db, &username).map_err(ApiError::from)
    })
    .flat_map({
      let app = Arc::clone(&app);
      let throttle = throttle.clone();
      let account = account.clone();
      move |user| {
        let stored = match user
          .as_ref()
          .map(|u| PasswordHash::parse(u.password_hash.clone()))
        {
          Some(Err(e)) => return fail(ApiError::from(e)),
          Some(Ok(hash)) => Some(hash),
          None => None,
        };
        // An unknown user costs the same argon2 work as a wrong password.
        app
          .hasher
          .verify_or_dummy(password, stored.as_ref())
          .map_err(ApiError::from)
          .flat_map(move |verdict| match (user, verdict) {
            (Some(user), Verification::Valid) => succeed(user),
            (Some(user), Verification::ValidNeedsRehash(upgraded)) => app
              .db
              .execute(
                "UPDATE users SET password_hash = $1 WHERE username = $2",
                &params![upgraded.as_str(), user.username.as_str()],
              )
              .map(move |_| user)
              .map_err(ApiError::from),
            // A wrong password or an unknown user counts against the account and the address.
            _ => throttle
              .record_failure(&account, client)
              .map_err(ApiError::from)
              .flat_map(|()| fail(ApiError::BadCredentials("wrong username or password"))),
          })
      }
    })
    .flat_map(move |user| {
      // The organisation a login names (its rate-limit bucket) must be the user's own.
      if hint.is_some() && hint != user.org_id {
        return fail(ApiError::BadCredentials("wrong username or password"));
      }
      match signup::may_sign_in(user.email.as_deref(), user.email_verified) {
        Ok(()) => succeed(user),
        Err(why) => fail(ApiError::BadCredentials(why)),
      }
    })
    .flat_map({
      let app = Arc::clone(&app);
      move |user| second_factor(&app, user, totp)
    })
    .flat_map(move |(user, mfa)| sign_in(&app, &user, mfa, presented, StatusCode::OK))
    .flat_map(move |response| {
      throttle
        .record_success(&account)
        .map_err(ApiError::from)
        .map(move |()| response)
    })
    .catch(|e| match e {
      ApiError::Auth(AuthError::TooManyAttempts { retry_after }) => {
        succeed(too_many_attempts(retry_after))
      }
      e => fail(e),
    })
    .narrow()
}

/// A throttled login: `429` with `Retry-After` in whole seconds (at least 1), as
/// id_effect_auth's middleware answers [`AuthError::TooManyAttempts`].
fn too_many_attempts(retry_after: std::time::Duration) -> Response {
  // `Ord::max`: sea_query's `ExprTrait` (imported here) has a `max` too.
  let secs = Ord::max(
    retry_after
      .as_secs()
      .saturating_add(u64::from(retry_after.subsec_nanos() > 0)),
    1,
  );
  let mut response = ApiError::Auth(AuthError::TooManyAttempts { retry_after }).into_response();
  response
    .headers_mut()
    .insert(header::RETRY_AFTER, HeaderValue::from(secs));
  response
}

/// The TOTP step of a login: none before enrolment; afterwards a valid, unused code is required.
fn second_factor(
  app: &App,
  user: UserRow,
  code: Option<String>,
) -> Effect<(UserRow, bool), ApiError> {
  let Some(secret) = user.totp_secret.clone() else {
    return succeed((user, false));
  };
  let Some(code) = code else {
    return fail(ApiError::BadCredentials("a TOTP code is required"));
  };
  let secret = match TotpSecret::from_base32(&secret) {
    Ok(secret) => secret,
    Err(e) => return fail(ApiError::from(e)),
  };
  app
    .totp
    .verify(&user.username, &secret, &code)
    .map_err(ApiError::from)
    .flat_map(move |verdict| match verdict {
      TotpVerification::Valid => succeed((user, true)),
      _ => fail(ApiError::BadCredentials("wrong or reused TOTP code")),
    })
}

fn logout(
  env: &mut AppEnv,
  _caps: RequestCaps,
  input: Input<(), (), CookieHeader>,
) -> Effect<Response, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let cleared = app.sessions.clear_cookie();
  let ended = match presented_session(&app, &input.headers) {
    Some(id) => app.sessions.logout(&id).map(|_| ()).map_err(ApiError::from),
    None => succeed(()),
  };
  ended
    .map(move |()| (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cleared)]).into_response())
    .narrow()
}

fn me(_: &mut AppEnv, caps: RequestCaps, _: Input) -> Effect<Json<Me>, ApiError, AppEnv> {
  as_caller(&caps, |caller| {
    let principal = caller.principal();
    succeed(Json(Me {
      id: caller.id().to_owned(),
      org: caller.org().to_owned(),
      roles: principal.roles().to_vec(),
      mfa: principal.permissions().iter().any(|p| p == MFA),
    }))
  })
}

/// The live session of the request and its password user; `404` for a caller without one (an
/// API token, an OpenID Connect login).
fn session_user(
  app: &App,
  caller: &Caller,
  headers: &CookieHeader,
) -> Effect<(Session, UserRow), ApiError> {
  let Some(id) = presented_session(app, headers) else {
    return fail(ApiError::NotFound("password session"));
  };
  let db = Arc::clone(&app.db);
  let username = caller.id().to_owned();
  app
    .sessions
    .load(&id)
    .map_err(ApiError::from)
    .flat_map(move |session| {
      find_user(&db, &username)
        .map_err(ApiError::from)
        .flat_map(move |user| match (session, user) {
          (Some(session), Some(user)) => succeed((session, user)),
          _ => fail(ApiError::NotFound("password session")),
        })
    })
}

fn enrol_totp(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), CookieHeader>,
) -> Effect<Json<Enrolment>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    session_user(&app, &caller, &input.headers).flat_map(move |(mut session, user)| {
      if user.totp_secret.is_some() {
        return fail(ApiError::Conflict("TOTP is already enrolled".into()));
      }
      let enrolment = match app.totp.enrol(&user.username) {
        Ok(enrolment) => enrolment,
        Err(e) => return fail(ApiError::from(e)),
      };
      let secret = enrolment.secret().to_base32();
      session.set_attr(TOTP_PENDING_ATTR, secret.clone());
      let shown = Enrolment {
        secret,
        uri: enrolment.uri().to_owned(),
      };
      app
        .sessions
        .save(session)
        .map(move |()| Json(shown))
        .map_err(ApiError::from)
    })
  })
}

fn confirm_totp(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), CookieHeader, TotpCode>,
) -> Effect<StatusCode, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let code = input.body.code;
  as_caller(&caps, move |caller| {
    session_user(&app, &caller, &input.headers).flat_map(move |(mut session, user)| {
      let Some(pending) = session.attr(TOTP_PENDING_ATTR).map(str::to_owned) else {
        return fail(ApiError::Conflict("start the enrolment first".into()));
      };
      let secret = match TotpSecret::from_base32(&pending) {
        Ok(secret) => secret,
        Err(e) => return fail(ApiError::from(e)),
      };
      let (db, sessions) = (Arc::clone(&app.db), app.sessions.clone());
      app
        .totp
        .verify(&user.username, &secret, &code)
        .map_err(ApiError::from)
        .flat_map(move |verdict| {
          if verdict != TotpVerification::Valid {
            return fail(ApiError::BadCredentials("wrong or reused TOTP code"));
          }
          session.remove_attr(TOTP_PENDING_ATTR);
          db.execute(
            "UPDATE users SET totp_secret = $1 WHERE username = $2",
            &params![pending.as_str(), user.username.as_str()],
          )
          .map_err(ApiError::from)
          .flat_map(move |_| sessions.save(session).map_err(ApiError::from))
          .map(|()| StatusCode::NO_CONTENT)
        })
    })
  })
}

fn create_user(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), (), NewUser>,
) -> Effect<(StatusCode, Json<UserCreated>), ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let NewUser {
    username,
    password,
    role,
  } = input.body;
  as_caller(&caps, move |caller| {
    let db = Arc::clone(&app.db);
    app
      .hasher
      .hash(password)
      .map_err(ApiError::from)
      .flat_map(move |hash| {
        let row = params![
          caller.org(),
          username.as_str(),
          hash.as_str(),
          role.as_str()
        ];
        // In a transaction: a taken username is a SQLSTATE 23505, answered 409.
        with_transaction(db, move |tx| {
          tx.execute(
            "INSERT INTO users (org_id, username, password_hash, role) VALUES ($1, $2, $3, $4)",
            &row,
          )
        })
        .map(move |_| (StatusCode::CREATED, Json(UserCreated { username, role })))
        .map_err(ApiError::from)
      })
  })
}

fn issue_token(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), (), NewToken>,
) -> Effect<(StatusCode, Json<TokenCreated>), ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let name = input.body.name;
  as_caller(&caps, move |caller| {
    if !(1..=80).contains(&name.chars().count()) {
      return fail(ApiError::invalid("/name", "must be 1 to 80 characters"));
    }
    let principal = AuthPrincipal::new(caller.id())
      .with_roles(caller.principal().roles().iter().cloned())
      .with_tenant(caller.org());
    let expires_in = i64::try_from(app.token_ttl.as_secs()).unwrap_or(i64::MAX);
    let verifier = app.verifier.clone();
    app
      .tokens
      .issue(&principal)
      // Read back the `jti` the issuer assigned: it identifies the token for revocation.
      .flat_map(move |token| verifier.verify(&token).map(move |claims| (token, claims)))
      .map_err(ApiError::from)
      .flat_map(move |(token, claims)| {
        let id = claims
          .claim("jti")
          .and_then(|jti| jti.as_str())
          .unwrap_or_default()
          .to_owned();
        let record = ApiToken {
          id: id.clone(),
          org_id: caller.org().to_owned(),
          owner_id: caller.id().to_owned(),
          name: name.clone(),
        };
        tokens_of(caller.org())
          .on(Arc::clone(&app.db))
          .insert(record)
          .map_err(ApiError::from)
          .map(move |_| {
            let created = TokenCreated {
              id,
              name,
              token,
              expires_in,
            };
            (StatusCode::CREATED, Json(created))
          })
      })
  })
}

fn list_tokens(
  env: &mut AppEnv,
  caps: RequestCaps,
  _: Input,
) -> Effect<Json<TokenList>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    tokens_of(caller.org())
      .on(Arc::clone(&app.db))
      .find_by(Expr::col("owner_id").eq(caller.id()))
      .map(|tokens| Json(TokenList { tokens }))
      .map_err(ApiError::from)
  })
}

fn revoke_token(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<String>,
) -> Effect<StatusCode, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let id = input.path;
  as_caller(&caps, move |caller| {
    let tokens = tokens_of(caller.org()).on(Arc::clone(&app.db));
    tokens
      .find(id.clone())
      .map_err(ApiError::from)
      .flat_map(move |found| match found {
        None => fail(ApiError::NotFound("API token")),
        Some(token) => caller
          .authorize(&app.policies, "token.revoke", &token)
          .flat_map(move |()| tokens.delete(id).map_err(ApiError::from))
          .map(|_| StatusCode::NO_CONTENT),
      })
  })
}

/// Middleware after authentication: a bearer token whose id was revoked (or never issued
/// here) is refused with `401`, whatever its signature says.
pub async fn reject_revoked_tokens(
  State(app): State<App>,
  request: Request,
  next: Next,
) -> Response {
  let jti = request
    .extensions()
    .get::<AuthPrincipal>()
    .filter(|p| p.scheme() == Some(AuthScheme::Bearer))
    .and_then(|p| Some((p.claim("jti")?.as_str()?.to_owned(), p.tenant()?.to_owned())));
  if let Some((jti, org)) = jti {
    let live = tokens_of(&org).on(Arc::clone(&app.db)).exists(jti);
    match run_with_env(HNil, move |_| live).await {
      Exit::Success(true) => {}
      Exit::Success(false) => {
        return ApiError::BadCredentials("the API token was revoked").into_response();
      }
      Exit::Failure(_) => {
        return ApiError::Sql(SqlError::NotConnected).into_response();
      }
    }
  }
  next.run(request).await
}

/// Sign-in routes; `login_limit` throttles `POST /orgs` and `POST /auth/login`, per client and
/// per organisation. Those two are public (anonymous callers sign up and sign in); every other
/// route names its policy.
pub fn routes(policies: &Policies, login_limit: LoginLimit) -> Router<AppEnv> {
  Router::new()
    .route(
      Route::post("/orgs")
        .public()
        .headers::<CookieHeader>()
        .body::<Register>()
        .response::<Me>(StatusCode::CREATED)
        .handler_with_request_caps(register)
        .layer(login_limit.client.clone()),
    )
    .route(
      Route::post("/auth/login")
        .public()
        .headers::<CookieHeader>()
        .body::<Login>()
        .response::<Me>(StatusCode::OK)
        .handler_with_request_caps(login)
        .layer(login_limit.tenant)
        .layer(login_limit.client),
    )
    .route(guarded(
      policies,
      "signed_in",
      Route::post("/auth/logout")
        .headers::<CookieHeader>()
        .handler_with_request_caps(logout),
    ))
    .route(guarded(
      policies,
      "signed_in",
      Route::get("/me")
        .response::<Me>(StatusCode::OK)
        .handler_with_request_caps(me),
    ))
    .route(guarded(
      policies,
      "signed_in",
      Route::post("/auth/totp/enrol")
        .headers::<CookieHeader>()
        .response::<Enrolment>(StatusCode::OK)
        .handler_with_request_caps(enrol_totp),
    ))
    .route(guarded(
      policies,
      "signed_in",
      Route::post("/auth/totp/confirm")
        .headers::<CookieHeader>()
        .body::<TotpCode>()
        .handler_with_request_caps(confirm_totp),
    ))
    .route(guarded(
      policies,
      "users.manage",
      Route::post("/users")
        .body::<NewUser>()
        .response::<UserCreated>(StatusCode::CREATED)
        .handler_with_request_caps(create_user),
    ))
    .route(guarded(
      policies,
      "tokens.issue",
      Route::post("/auth/tokens")
        .body::<NewToken>()
        .response::<TokenCreated>(StatusCode::CREATED)
        .handler_with_request_caps(issue_token),
    ))
    .route(guarded(
      policies,
      "signed_in",
      Route::get("/auth/tokens")
        .response::<TokenList>(StatusCode::OK)
        .handler_with_request_caps(list_tokens),
    ))
    .route(guarded(
      policies,
      "signed_in",
      Route::delete("/auth/tokens/{id}")
        .path::<TokenId>()
        .handler_with_request_caps(revoke_token),
    ))
}
