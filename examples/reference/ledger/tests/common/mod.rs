//! The test harness: every test gets a PostgreSQL database of its own (a clone of an empty
//! template on a containerised server), migrated by the app's own start-up path, and the whole
//! app — middleware included — behind the in-process `TestClient`.
//!
//! Without Docker a test prints `SKIP` and passes; with `ID_EFFECT_REQUIRE_DOCKER=1` (CI) it
//! fails instead.

#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use id_effect::HNil;
use id_effect::{Cause, Effect, Exit, UtcDateTime, caps, from_async, need, succeed};
use id_effect_kit::auth::{
  MemoryTotpStepStore, PasswordHasher, PasswordParams, Totp, TotpConfig, TotpSecret,
};
use id_effect_kit::authz::{AuthzEvents, MemorySink};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::mail::{MailerService, SmtpConfig, SmtpMailer};
use id_effect_kit::sql::{SqlClient, SqlClientService, SqlError, SqlValue, params};
use id_effect_kit::storage::ObjectStorage;
use id_effect_kit::test::fixtures::{BoxedStrategy, Strategy, from_schema};
use id_effect_kit::test::http::{StatusCode, TestClient, TestRequest, TestResponse};
use id_effect_kit::test::{
  ContainerError, ContainerImage, Factory, Fixture, IsolatedDatabase, Mailpit, MailpitContainer,
  Postgres, S3, S3Container, Shared, TemplateDb,
};
use ledger::accounts::Account;
use ledger::config::{self, LedgerConfig, StorageConfig};
use ledger::{Database, Services, StartupError};
use serde_json::{Value, json};

/// The app's migrations.
pub const MIGRATIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");
/// The app's profiles.
pub const CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/config");
/// The app's mail templates.
pub const TEMPLATES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/templates/mail");
/// The header a browser echoes the CSRF token in.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// The password every fixture user has.
pub const PASSWORD: &str = "correct horse battery staple";

/// One PostgreSQL server per test process.
pub static PG: Shared<Postgres> = Shared::new(Postgres::new);
/// Empty: the app migrates each test database itself when it starts, as in production.
pub static EMPTY: TemplateDb = TemplateDb::new(|_db| succeed(()));

/// Why a test's setup failed.
#[derive(Debug)]
pub enum TestError {
  Container(ContainerError),
  Sql(SqlError),
  Startup(StartupError),
}

impl From<ContainerError> for TestError {
  fn from(e: ContainerError) -> Self {
    Self::Container(e)
  }
}

impl From<SqlError> for TestError {
  fn from(e: SqlError) -> Self {
    Self::Sql(e)
  }
}

impl From<StartupError> for TestError {
  fn from(e: StartupError) -> Self {
    Self::Startup(e)
  }
}

/// Runs `eff` on the serving runtime and returns its value, panicking on any failure.
pub async fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(eff: Effect<A, E>) -> A {
  match EffectExecutor::global().run_effect(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("effect failed: {cause:?}"),
  }
}

/// The `test` profile, with the migrations directory made absolute.
pub async fn test_config() -> LedgerConfig {
  let profiles = config::profiles(CONFIG);
  let profile = profiles.profile("test").expect("test profile");
  let mut config = run(config::load(&profiles, profile, None)).await;
  config.db.migrations = MIGRATIONS.to_owned();
  config.mail.templates = TEMPLATES.to_owned();
  config
}

/// The CSRF cookie and token a browser holds (from `GET /auth/csrf`).
#[derive(Clone, Debug, Default)]
pub struct Csrf {
  /// `__Host-csrf=…`.
  pub cookie: String,
  /// What the browser echoes in [`CSRF_HEADER`].
  pub token: String,
}

impl Csrf {
  /// A browser's first visit to `client`: the CSRF cookie the security layer sets and its
  /// token.
  pub async fn fetch(client: &TestClient<axum::Router>) -> Self {
    let res = run(client.get("/auth/csrf").send()).await;
    res.assert_status(StatusCode::OK);
    let cookie = res
      .header("set-cookie")
      .expect("a CSRF cookie")
      .split(';')
      .next()
      .expect("name=value")
      .to_owned();
    let token = res.json::<Value>().expect("json")["token"]
      .as_str()
      .expect("a token")
      .to_owned();
    Self { cookie, token }
  }
}

/// A running ledger over a fresh database.
pub struct Ledger {
  /// The whole app, in process.
  pub client: TestClient<axum::Router>,
  /// The database (for fixtures and for checking what the app wrote).
  pub db: Arc<dyn SqlClient>,
  /// Where the test database is (for a second app instance or a replica).
  pub url: String,
  /// The configuration the app runs with.
  pub config: LedgerConfig,
  /// Every authorization decision the app made.
  pub decisions: MemorySink,
  /// The browser's CSRF cookie and token, sent with every request but bearer ones.
  pub csrf: Csrf,
}

/// Runs `test` against a ledger on a database of its own. `configure` may change the
/// configuration and the services (Redis, a replica, …) before the app is built.
pub async fn with_ledger<C, F, Fut>(configure: C, test: F)
where
  C: FnOnce(&mut LedgerConfig, &mut Services) + Send + 'static,
  F: FnOnce(Ledger) -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let config = test_config().await;
  let program: Effect<(), TestError, caps!(SqlClientService, IsolatedDatabase)> =
    need::<SqlClientService>().flat_map(move |db| {
      need::<IsolatedDatabase>().flat_map(move |isolated| {
        let db = Arc::clone(&db.0);
        let url = isolated.url();
        // The start-up path of the service: migrations through the `migrate` layer.
        ledger::db::migrate(Arc::clone(&db), config.db.migrations.clone())
          .map_err(TestError::from)
          .flat_map(move |_applied| {
            from_async(move || async move {
              let mut ledger = Ledger::start(db, url, config, configure);
              ledger.csrf = Csrf::fetch(&ledger.client).await;
              test(ledger).await;
              Ok::<(), TestError>(())
            })
          })
          .narrow()
      })
    });
  let databases = PG.layer::<TestError>().provide_to(EMPTY.isolated_db());
  match EffectExecutor::global()
    .run_effect(program.provide_layer(databases))
    .await
  {
    Exit::Success(()) => {}
    Exit::Failure(Cause::Die(defect)) => panic!("{}", defect.message()),
    Exit::Failure(cause) => panic!("test failed: {cause:?}"),
  }
}

/// Runs `test` with a fresh, empty database of its own (nothing migrated) and its URL.
pub async fn with_database<F, Fut>(test: F)
where
  F: FnOnce(Arc<dyn SqlClient>, String) -> Fut + Send + 'static,
  Fut: Future<Output = ()> + Send + 'static,
{
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let program: Effect<(), TestError, caps!(SqlClientService, IsolatedDatabase)> =
    need::<SqlClientService>().flat_map(move |db| {
      need::<IsolatedDatabase>().flat_map(move |isolated| {
        let (db, url) = (Arc::clone(&db.0), isolated.url());
        let body: Effect<(), TestError> = from_async(move || async move {
          test(db, url).await;
          Ok::<(), TestError>(())
        });
        body.narrow()
      })
    });
  let databases = PG.layer::<TestError>().provide_to(EMPTY.isolated_db());
  match EffectExecutor::global()
    .run_effect(program.provide_layer(databases))
    .await
  {
    Exit::Success(()) => {}
    Exit::Failure(Cause::Die(defect)) => panic!("{}", defect.message()),
    Exit::Failure(cause) => panic!("test failed: {cause:?}"),
  }
}

/// A PostgreSQL client over `url`.
pub async fn connect(url: &str) -> Arc<dyn SqlClient> {
  use id_effect_kit::sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  let pool = pg_pool_from_config(PgPoolConfig::from_url(url))
    .await
    .unwrap_or_else(|e| panic!("connect {url}: {:?}", e.0));
  Arc::new(PgSqlClient::new(pool))
}

impl Ledger {
  fn start<C>(db: Arc<dyn SqlClient>, url: String, mut config: LedgerConfig, configure: C) -> Self
  where
    C: FnOnce(&mut LedgerConfig, &mut Services),
  {
    let decisions = MemorySink::new();
    let mut services = Services {
      events: AuthzEvents::new(decisions.clone()),
      ..Services::new(Database::new(Arc::clone(&db), vec![]))
    };
    configure(&mut config, &mut services);
    let app = ledger::router(&config, services).expect("the app builds");
    Self {
      client: TestClient::new(app),
      db,
      url,
      config,
      decisions,
      csrf: Csrf::default(),
    }
  }

  /// Sends `request` and reads the whole response.
  pub async fn send(&self, request: TestRequest<axum::Router>) -> TestResponse {
    run(request.send()).await
  }

  /// `request` as a browser sends it: the CSRF cookie (after `session`, a `name=value` pair,
  /// when given) and the CSRF header.
  pub fn browser(
    &self,
    request: TestRequest<axum::Router>,
    session: Option<&str>,
  ) -> TestRequest<axum::Router> {
    let cookie = match session {
      Some(session) => format!("{session}; {}", self.csrf.cookie),
      None => self.csrf.cookie.clone(),
    };
    request
      .header("cookie", &cookie)
      .header(CSRF_HEADER, &self.csrf.token)
  }

  /// `request` sent by `who` (a session cookie or a bearer token, see [`As`]).
  pub fn by(&self, who: &As, request: TestRequest<axum::Router>) -> TestRequest<axum::Router> {
    match who {
      As::Anonymous => self.browser(request, None),
      As::Session(cookie) => self.browser(request, Some(cookie)),
      As::Bearer(token) => request.bearer(token),
    }
  }

  /// `GET path` as `who` (a session cookie or a bearer token, see [`As`]).
  pub async fn get(&self, path: &str, who: &As) -> TestResponse {
    self.send(self.by(who, self.client.get(path))).await
  }

  /// `POST path` with a JSON body as `who`.
  pub async fn post(&self, path: &str, body: Value, who: &As) -> TestResponse {
    self
      .send(self.by(who, self.client.post_json(path, &body)))
      .await
  }

  /// `PATCH path` with a JSON body as `who`.
  pub async fn patch(&self, path: &str, body: Value, who: &As) -> TestResponse {
    self
      .send(self.by(who, self.client.patch_json(path, &body)))
      .await
  }

  /// `PUT path` with a JSON body as `who`.
  pub async fn put(&self, path: &str, body: Value, who: &As) -> TestResponse {
    self
      .send(self.by(who, self.client.put_json(path, &body)))
      .await
  }

  /// `DELETE path` as `who`.
  pub async fn delete(&self, path: &str, who: &As) -> TestResponse {
    self.send(self.by(who, self.client.delete(path))).await
  }

  /// Runs `sql` on the test database.
  pub async fn execute(&self, sql: &str, params: Vec<SqlValue>) -> u64 {
    run(self.db.execute(sql, &params)).await
  }

  /// Runs `sql` on the test database, returning the database's refusal instead of panicking.
  pub async fn try_execute(&self, sql: &str, params: Vec<SqlValue>) -> Result<u64, SqlError> {
    let db = Arc::clone(&self.db);
    let sql = sql.to_owned();
    let attempt = id_effect::suspend(move || db.execute(&sql, &params));
    match EffectExecutor::global().run_effect(attempt).await {
      Exit::Success(n) => Ok(n),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("{cause:?}"),
    }
  }

  /// The one `i64` column of `sql`'s first row.
  pub async fn scalar(&self, sql: &str, params: Vec<SqlValue>) -> i64 {
    let rows = run(self.db.query(sql, &params)).await;
    rows[0].get::<i64>(0usize).expect("an int8 column")
  }

  /// An organisation.
  pub async fn org(&self, org: &str) {
    self
      .execute("INSERT INTO orgs (id) VALUES ($1)", params![org])
      .await;
  }

  /// A user of `org` with `role` and [`PASSWORD`], optionally enrolled in TOTP with `totp`.
  pub async fn user(&self, org: &str, username: &str, role: &str, totp: Option<&TotpSecret>) {
    let hasher = PasswordHasher::new(PasswordParams::new(64, 2, 1).expect("params"));
    let hash = run(hasher.hash(PASSWORD)).await;
    self
      .execute(
        "INSERT INTO users (org_id, username, password_hash, role, totp_secret) \
         VALUES ($1, $2, $3, $4, $5)",
        params![
          org,
          username,
          hash.as_str(),
          role,
          totp.map(TotpSecret::to_base32)
        ],
      )
      .await;
  }

  /// Gives `username` the verified address `email` (its statements are mailed there).
  pub async fn email(&self, username: &str, email: &str) {
    self
      .execute(
        "UPDATE users SET email = $2, email_verified = true WHERE username = $1",
        params![username, email],
      )
      .await;
  }

  /// Logs `username` in (with a TOTP code when given) and returns the session cookie.
  pub async fn login(&self, username: &str, totp: Option<String>) -> As {
    let res = self
      .post(
        "/auth/login",
        json!({"username": username, "password": PASSWORD, "totp": totp}),
        &As::Anonymous,
      )
      .await;
    res.assert_status(StatusCode::OK);
    As::session(&res)
  }

  /// An organisation with an owner, an accountant and a viewer (`<org>-owner`, …), each logged
  /// in.
  pub async fn team(&self, org: &str) -> Team {
    self.org(org).await;
    for role in ["owner", "accountant", "viewer"] {
      self.user(org, &format!("{org}-{role}"), role, None).await;
    }
    Team {
      owner: self.login(&format!("{org}-owner"), None).await,
      accountant: self.login(&format!("{org}-accountant"), None).await,
      viewer: self.login(&format!("{org}-viewer"), None).await,
    }
  }

  /// Accounts of `org` opened by `owner_id`, from the [`NewAccount`] factory.
  pub async fn accounts(&self, org: &str, owner_id: &str, n: usize) -> Vec<Account> {
    let (org, owner_id) = (org.to_owned(), owner_id.to_owned());
    let mut factory = Factory::<NewAccount>::seeded(7).with_seq(move |a, seq| {
      a.org_id = org.clone();
      a.owner_id = owner_id.clone();
      a.code = format!("{}", 1000 + seq);
    });
    let insert: Effect<Vec<Account>, SqlError, caps!(SqlClientService)> = factory.insert_many(n);
    run(insert.provide_all(id_effect::hlist![SqlClientService(Arc::clone(&self.db))])).await
  }

  /// Posts a balanced entry moving `amount` from `credit` to `debit`; its id.
  pub async fn transfer(&self, who: &As, debit: i64, credit: i64, amount: i64) -> i64 {
    let res = self
      .post(
        "/entries",
        json!({"memo": "transfer", "lines": [
          {"account_id": debit, "amount": amount},
          {"account_id": credit, "amount": -amount},
        ]}),
        who,
      )
      .await;
    res.assert_status(StatusCode::CREATED);
    res.json::<Value>().expect("json")["id"]
      .as_i64()
      .expect("id")
  }
}

/// A Mailpit server and an SMTP transport to it.
pub async fn mailpit() -> (MailpitContainer, MailerService) {
  let mailpit = run(Mailpit::new().start::<ContainerError, HNil>()).await;
  let smtp = SmtpConfig::plaintext(mailpit.host(), mailpit.smtp_port());
  let mailer = MailerService::new(SmtpMailer::new(smtp).expect("an SMTP mailer"));
  (mailpit, mailer)
}

/// `GET path` of Mailpit's API, as JSON.
pub async fn mailpit_api(mailpit: &MailpitContainer, path: &str) -> Value {
  let body = reqwest::get(format!("{}{path}", mailpit.api_url()))
    .await
    .expect("Mailpit reachable")
    .text()
    .await
    .expect("a body");
  serde_json::from_str(&body).unwrap_or_else(|e| panic!("{path}: {e}: {body}"))
}

/// The raw body of `GET path` of Mailpit's API.
pub async fn mailpit_raw(mailpit: &MailpitContainer, path: &str) -> String {
  reqwest::get(format!("{}{path}", mailpit.api_url()))
    .await
    .expect("Mailpit reachable")
    .text()
    .await
    .expect("a body")
}

/// Every message Mailpit holds, once there are `n` (newest first), each in full.
pub async fn mail_received(mailpit: &MailpitContainer, n: usize) -> Vec<Value> {
  for _ in 0..100 {
    let list = mailpit_api(mailpit, "/api/v1/messages").await;
    let messages = list["messages"].as_array().cloned().unwrap_or_default();
    if messages.len() >= n {
      let mut full = Vec::new();
      for m in messages {
        let id = m["ID"].as_str().expect("an id").to_owned();
        full.push(mailpit_api(mailpit, &format!("/api/v1/message/{id}")).await);
      }
      return full;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  panic!("Mailpit did not receive {n} message(s)");
}

/// The token of the link in `message`'s text (`…?token=<token>`).
pub fn link_token(message: &Value) -> String {
  let text = message["Text"].as_str().expect("a text body");
  let at = text.find("token=").expect("a link with a token") + "token=".len();
  text[at..]
    .chars()
    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
    .collect()
}

/// An S3-compatible server with the bucket `bucket`, and the Ledger's store on it.
pub async fn s3(bucket: &str) -> (S3Container, ObjectStorage, StorageConfig) {
  let s3 = run(S3::new().start::<ContainerError, HNil>()).await;
  create_bucket(&s3, bucket).await;
  let config = StorageConfig {
    bucket: Some(bucket.to_owned()),
    endpoint: Some(s3.endpoint()),
    region: "us-east-1".to_owned(),
    access_key_id: Some(s3.access_key_id().to_owned()),
    secret_access_key: Some(id_effect_kit::config::Secret::new(
      s3.secret_access_key().to_owned(),
    )),
    allow_http: true,
  };
  let store = ledger::app::statement_store(&config).expect("an S3 store");
  (s3, store, config)
}

/// `PUT /<bucket>` (CreateBucket), presigned with object_store's SigV4 signer: the Ledger reads
/// and writes objects, it never creates buckets.
pub async fn create_bucket(s3: &S3Container, bucket: &str) {
  use object_store::signer::Signer as _;
  let signer = object_store::aws::AmazonS3Builder::new()
    .with_endpoint(s3.endpoint())
    .with_allow_http(true)
    .with_bucket_name(bucket)
    .with_region("us-east-1")
    .with_access_key_id(s3.access_key_id())
    .with_secret_access_key(s3.secret_access_key())
    .build()
    .expect("a signer");
  let url = signer
    .signed_url(
      object_store::signer::Method::PUT,
      &object_store::path::Path::from(""),
      Duration::from_secs(60),
    )
    .await
    .expect("a presigned URL");
  let created = reqwest::Client::new()
    .put(url)
    .send()
    .await
    .expect("S3 reachable");
  assert!(
    created.status().is_success(),
    "create bucket {bucket}: {}",
    created.status()
  );
}

/// The three users of [`Ledger::team`], signed in.
pub struct Team {
  pub owner: As,
  pub accountant: As,
  pub viewer: As,
}

/// Who sends a request.
#[derive(Clone, Debug)]
pub enum As {
  /// No credential.
  Anonymous,
  /// A session cookie (`name=value`).
  Session(String),
  /// A bearer token.
  Bearer(String),
}

impl As {
  /// The session a sign-in response set.
  pub fn session(res: &TestResponse) -> Self {
    let set_cookie = res
      .headers()
      .get_all("set-cookie")
      .iter()
      .filter_map(|v| v.to_str().ok())
      .find(|c| !c.starts_with("__Host-csrf="))
      .expect("a session cookie");
    Self::Session(set_cookie.split(';').next().expect("name=value").to_owned())
  }
}

/// The factory's account: schema-valid names and kinds drawn from a seeded generator; the
/// tests set the organisation, the opener and the code.
#[derive(Clone, Debug)]
pub struct NewAccount {
  pub org_id: String,
  pub code: String,
  pub name: String,
  pub kind: String,
  pub owner_id: String,
}

impl Fixture for NewAccount {
  type Row = Account;

  fn arbitrary() -> BoxedStrategy<Self> {
    use id_effect::schema::{self, length, one_of};
    let name = from_schema(
      length(schema::string::<()>(), 1, 80),
      "[A-Z][a-z]{2,12}( [a-z]{2,8})?",
    );
    let kind = from_schema(
      one_of(schema::string::<()>(), &ledger::accounts::KINDS),
      "(asset|liability|equity|income|expense)",
    );
    (name, kind)
      .prop_map(|(name, kind)| NewAccount {
        org_id: String::new(),
        code: String::new(),
        name,
        kind,
        owner_id: String::new(),
      })
      .boxed()
  }

  fn insert_sql(&self) -> (String, Vec<SqlValue>) {
    (
      "INSERT INTO accounts (org_id, code, name, kind, owner_id) VALUES ($1, $2, $3, $4, $5) \
       RETURNING id, org_id, code, name, kind, owner_id, locked, version"
        .to_owned(),
      params![
        self.org_id.as_str(),
        self.code.as_str(),
        self.name.as_str(),
        self.kind.as_str(),
        self.owner_id.as_str()
      ],
    )
  }
}

/// The code an authenticator holding `secret` shows `steps` 30-second steps from now (`0`: now;
/// `1`: the next step, still inside the ±1 step window and later than a code used now).
pub fn totp_code(secret: &TotpSecret, steps: u64) -> String {
  let totp = Totp::new(TotpConfig::new("Ledger"), MemoryTotpStepStore::new());
  let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock");
  let at = now + Duration::from_secs(30 * steps);
  let at = UtcDateTime::unsafe_make(i64::try_from(at.as_millis()).expect("millis"));
  totp.code_at(secret, at).expect("code")
}
