//! `#[derive(FromRow)]` and the `query_as` helpers over [`TestSqlClient`] (data.sql#3).

use id_effect::schema::{self, HasSchema, ParseError, Schema};
use id_effect::{Effect, Exit};
use id_effect_sql::jiff::Timestamp;
use id_effect_sql::uuid::Uuid;
use id_effect_sql::{
  FromRow, SqlClientEnv, SqlError, SqlRow, SqlType, SqlValue, TestSqlClient, params, query_as,
  query_one_as, query_opt_as, sql_client_layer,
};

#[derive(Debug, PartialEq, FromRow)]
struct Audit {
  created_at: Timestamp,
  #[from_row(rename = "created_by")]
  author: Option<String>,
}

#[derive(Debug, PartialEq, FromRow)]
struct Todo {
  id: Uuid,
  #[from_row(rename = "title_text")]
  title: String,
  done: Option<bool>,
  #[from_row(default)]
  priority: i32,
  #[from_row(flatten)]
  audit: Audit,
  r#type: String,
}

fn todo_row() -> SqlRow {
  SqlRow::from_pairs([
    ("id", SqlValue::Uuid(Uuid::from_u128(7))),
    ("title_text", "docs".into()),
    ("done", SqlValue::Null),
    ("created_at", SqlValue::Timestamp(Timestamp::UNIX_EPOCH)),
    ("created_by", "ada".into()),
    ("type", "chore".into()),
  ])
}

fn todo() -> Todo {
  Todo {
    id: Uuid::from_u128(7),
    title: "docs".into(),
    done: None,
    priority: 0,
    audit: Audit {
      created_at: Timestamp::UNIX_EPOCH,
      author: Some("ada".into()),
    },
    r#type: "chore".into(),
  }
}

#[test]
fn derive_maps_columns_renames_nulls_defaults_and_flatten() {
  assert_eq!(Todo::from_row(&todo_row()), Ok(todo()));
  let mut pairs: Vec<(String, SqlValue)> = todo_row()
    .columns()
    .iter()
    .map(|c| c.name().to_owned())
    .zip(todo_row().into_values())
    .collect();
  pairs.push(("priority".into(), SqlValue::I64(3)));
  let decoded = Todo::from_row(&SqlRow::from_pairs(pairs)).unwrap();
  assert_eq!(decoded.priority, 3, "a present `default` column is decoded");
}

#[test]
fn derive_errors_name_the_column() {
  let missing = SqlRow::from_pairs([("id", SqlValue::Uuid(Uuid::nil()))]);
  assert_eq!(
    Todo::from_row(&missing),
    Err(SqlError::ColumnNotFound {
      column: "title_text".into()
    })
  );
  let wrong = SqlRow::from_pairs([
    ("created_at", SqlValue::I64(1)),
    ("created_by", SqlValue::Null),
  ]);
  assert_eq!(
    Audit::from_row(&wrong),
    Err(SqlError::TypeMismatch {
      column: "created_at".into(),
      expected: SqlType::Timestamp,
      found: SqlType::I64,
    })
  );
}

/// `#[schema("..")]` renames are the column names, with or without schema mode.
#[derive(Clone, Debug, PartialEq, id_effect::Schema, FromRow)]
struct Account {
  #[schema("account_id")]
  id: i64,
  name: String,
}

#[derive(Clone, Debug, PartialEq, id_effect::Schema, FromRow)]
#[from_row(schema)]
struct SchemaAccount {
  #[schema("account_id")]
  id: i64,
  nickname: Option<String>,
}

#[test]
fn schema_renames_are_column_names() {
  let row = SqlRow::from_pairs([("account_id", SqlValue::I32(5)), ("name", "ada".into())]);
  assert_eq!(
    Account::from_row(&row),
    Ok(Account {
      id: 5,
      name: "ada".into()
    })
  );
  assert_eq!(
    SchemaAccount::from_row(&row),
    Ok(SchemaAccount {
      id: 5,
      nickname: None
    })
  );
  let bad = SqlRow::from_pairs([("account_id", SqlValue::Text("x".into()))]);
  assert_eq!(
    SchemaAccount::from_row(&bad),
    Err(SqlError::InvalidRow {
      issues: vec![ParseError::new("account_id", "expected i64")]
    })
  );
}

/// A hand-written schema with an accumulating decoder and a refinement on every field.
#[derive(Debug, PartialEq, FromRow)]
#[from_row(schema)]
struct Signup {
  age: i64,
  email: String,
}

impl HasSchema for Signup {
  type A = Signup;
  type I = (i64, String);
  type E = ();

  fn schema() -> Schema<Signup, (i64, String), ()> {
    schema::transform(
      schema::struct_(
        "age",
        schema::refine(schema::i64(), |n| *n >= 18, "must be an adult"),
        "email",
        schema::refine(schema::string(), |s| s.contains('@'), "must be an email"),
      ),
      |(age, email)| Ok(Signup { age, email }),
      |s: Signup| (s.age, s.email),
    )
  }
}

#[test]
fn schema_mode_runs_refinements_and_reports_every_issue() {
  let ok = SqlRow::from_pairs([("age", SqlValue::I16(30)), ("email", "a@b".into())]);
  assert_eq!(
    Signup::from_row(&ok),
    Ok(Signup {
      age: 30,
      email: "a@b".into()
    })
  );
  let bad = SqlRow::from_pairs([("age", SqlValue::I64(12)), ("email", "nope".into())]);
  let err = Signup::from_row(&bad).unwrap_err();
  assert_eq!(
    err,
    SqlError::InvalidRow {
      issues: vec![
        ParseError::new("age", "must be an adult"),
        ParseError::new("email", "must be an email"),
      ]
    }
  );
  assert_eq!(
    err.to_string(),
    "sql row invalid: age: must be an adult; email: must be an email"
  );
  assert_eq!(
    SqlError::InvalidRow {
      issues: vec![ParseError::new("", "expected object")]
    }
    .to_string(),
    "sql row invalid: expected object"
  );
}

#[derive(Debug, PartialEq, FromRow)]
#[from_row(crate = id_effect_sql)]
struct Empty {}

#[derive(Debug, PartialEq, FromRow)]
struct Unit;

#[derive(Debug, PartialEq, FromRow)]
struct Wrapper<T: id_effect_sql::FromSqlValue> {
  value: T,
}

#[test]
fn unit_empty_and_generic_structs() {
  assert_eq!(Empty::from_row(&SqlRow::default()), Ok(Empty {}));
  assert_eq!(Unit::from_row(&SqlRow::default()), Ok(Unit));
  let row = SqlRow::from_pairs([("value", 4i64)]);
  assert_eq!(Wrapper::<i64>::from_row(&row), Ok(Wrapper { value: 4 }));
}

const SQL: &str = "SELECT * FROM todos WHERE id = $1";

fn run<A: Send + 'static>(
  test: &TestSqlClient,
  program: Effect<A, SqlError, SqlClientEnv>,
) -> Exit<A, SqlError> {
  program
    .provide_layer(sql_client_layer::<SqlError>(test.clone()))
    .block_on()
}

#[test]
fn query_helpers_decode_rows() {
  let test = TestSqlClient::new();
  test.script_query(SQL, vec![todo_row(), todo_row()]);
  let all = run(&test, query_as::<Todo, _, _>(SQL, params![7i64]));
  assert!(matches!(all, Exit::Success(ref v) if v.len() == 2 && v[0] == todo()));
  let one = run(&test, query_one_as::<Todo, _, _>(SQL, params![7i64]));
  assert!(matches!(one, Exit::Success(ref t) if *t == todo()));
  let opt = run(&test, query_opt_as::<(Uuid,), _, _>(SQL, params![7i64]));
  assert!(matches!(opt, Exit::Success(Some((id,))) if id == Uuid::from_u128(7)));
  assert_eq!(test.recorded_queries()[0].1, [SqlValue::I64(7)]);
}

#[test]
fn query_helpers_report_missing_rows_and_decode_errors() {
  let test = TestSqlClient::new();
  test.script_query(SQL, vec![]);
  let none = run(&test, query_opt_as::<Todo, _, _>(SQL, params![1i64]));
  assert!(matches!(none, Exit::Success(None)));
  let one = run(&test, query_one_as::<Todo, _, _>(SQL, params![1i64]));
  assert!(matches!(one, Exit::Failure(ref c) if format!("{c:?}").contains("RowNotFound")));
  assert_eq!(
    SqlError::RowNotFound.to_string(),
    "sql query returned no row"
  );

  test.script_query(SQL, vec![SqlRow::from_pairs([("id", 1i64)])]);
  let bad = run(&test, query_as::<Todo, _, _>(SQL, params![1i64]));
  assert!(matches!(bad, Exit::Failure(ref c) if format!("{c:?}").contains("TypeMismatch")));
}

/// Live PostgreSQL: typed columns (uuid, timestamptz, NULL, int4) decode through the derive and
/// through a schema. Same policy as `tests/pg.rs`: without `DATABASE_URL` it prints `SKIP`,
/// and `ID_EFFECT_REQUIRE_PG=1` turns that into a failure.
#[cfg(feature = "pg")]
#[tokio::test]
async fn live_postgres_rows_decode_into_structs() {
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{SqlClient, provide_sql_client};
  use id_effect_tokio::TokioRuntime;

  let test = "live_postgres_rows_decode_into_structs";
  let required = std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1");
  let pool = match std::env::var("DATABASE_URL") {
    Ok(url) => pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await,
    Err(_) => {
      assert!(
        !required,
        "{test}: ID_EFFECT_REQUIRE_PG=1 but DATABASE_URL is not set"
      );
      eprintln!("SKIP {test}: DATABASE_URL is not set");
      return;
    }
  };
  let pool = match pool {
    Ok(pool) => pool,
    Err(e) => {
      assert!(!required, "{test}: cannot open a pool: {e:?}");
      eprintln!("SKIP {test}: cannot open a pool: {e:?}");
      return;
    }
  };
  let rt = TokioRuntime::current().expect("tokio");
  let client = PgSqlClient::new(pool);
  if let Exit::Failure(c) = rt.run(client.connect()).await {
    assert!(!required, "{test}: cannot connect: {c:?}");
    eprintln!("SKIP {test}: cannot connect: {c:?}");
    return;
  }

  let sql = "SELECT $1::uuid AS id, 'docs'::text AS title_text, NULL::bool AS done, \
             to_timestamp(0) AS created_at, 'ada'::text AS created_by, 'chore' AS type";
  let todos = query_as::<Todo, SqlClientEnv, _>(sql, params![Uuid::from_u128(7)])
    .provide_layer(provide_sql_client::<SqlError>(client.clone()));
  assert!(matches!(rt.run(todos).await, Exit::Success(v) if v == [todo()]));

  let account = query_one_as::<SchemaAccount, SqlClientEnv, _>(
    "SELECT 5::int4 AS account_id, NULL::text AS nickname",
    params![],
  )
  .provide_layer(provide_sql_client::<SqlError>(client.clone()));
  assert!(matches!(
    rt.run(account).await,
    Exit::Success(SchemaAccount {
      id: 5,
      nickname: None
    })
  ));

  let bad =
    query_one_as::<SchemaAccount, SqlClientEnv, _>("SELECT 'x'::text AS account_id", params![])
      .provide_layer(provide_sql_client::<SqlError>(client.clone()));
  assert!(matches!(
    rt.run(bad).await,
    Exit::Failure(id_effect::Cause::Fail(SqlError::InvalidRow { issues }))
      if issues[0].path == "account_id"
  ));
  let _ = rt.run(client.close()).await;
}

#[test]
// The expected rustc output lists `Value` (serde_json's); with `query`, sea-query's `Value` makes
// rustc print the full path, which sorts out of the shown impls.
#[cfg_attr(
  feature = "query",
  ignore = "rustc's impl list depends on the crate graph; run without `query`"
)]
fn derive_compile_errors() {
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/from_row/*.rs");
}
