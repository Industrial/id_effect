//! Unit tests of the sea-query bridge and the statement helpers.

use id_effect::{Cause, Exit};
use jiff::civil::{date, time as clock};
use proptest::prelude::*;
use rust_decimal::Decimal;
use sea_query::{
  Alias, Enum, Expr, ExprTrait, JoinType, OnConflict, OptionEnum, Order, Query, Value, Values,
};
use uuid::Uuid;

use super::*;
use crate::client::{SqlClientEnv, TestSqlClient, sql_client_layer};
use crate::row::SqlRow;

fn ts(s: &str) -> jiff::Timestamp {
  s.parse().unwrap()
}

fn utc(t: jiff::Timestamp) -> time::OffsetDateTime {
  time::OffsetDateTime::from_unix_timestamp_nanos(t.as_nanosecond()).unwrap()
}

/// Every sea-query variant this crate enables, `Some` and `None`, with its `SqlValue`.
fn table() -> Vec<(Value, SqlValue)> {
  let t = ts("2024-02-29T12:34:56.789012Z");
  let dt = date(2024, 2, 29).at(12, 34, 56, 789_012_000);
  let json = serde_json::json!({"a": [1, null, "x"]});
  let odt = utc(t).to_offset(time::UtcOffset::from_hms(5, 30, 0).unwrap());
  let pdt = time::PrimitiveDateTime::new(
    time::Date::from_calendar_date(2024, time::Month::February, 29).unwrap(),
    time::Time::from_hms_nano(12, 34, 56, 789_012_000).unwrap(),
  );
  let enum_ty: sea_query::EnumTypeName = "mood".into();
  vec![
    (Value::Bool(Some(true)), SqlValue::Bool(true)),
    (Value::TinyInt(Some(-8)), SqlValue::I16(-8)),
    (Value::SmallInt(Some(i16::MIN)), SqlValue::I16(i16::MIN)),
    (Value::Int(Some(i32::MAX)), SqlValue::I32(i32::MAX)),
    (Value::BigInt(Some(i64::MIN)), SqlValue::I64(i64::MIN)),
    (Value::TinyUnsigned(Some(u8::MAX)), SqlValue::I16(255)),
    (Value::SmallUnsigned(Some(u16::MAX)), SqlValue::I32(65_535)),
    (
      Value::Unsigned(Some(u32::MAX)),
      SqlValue::I64(i64::from(u32::MAX)),
    ),
    (
      Value::BigUnsigned(Some(i64::MAX as u64)),
      SqlValue::I64(i64::MAX),
    ),
    (Value::Float(Some(1.5)), SqlValue::F32(1.5)),
    (Value::Double(Some(-2.25)), SqlValue::F64(-2.25)),
    (
      Value::String(Some("ada".into())),
      SqlValue::Text("ada".into()),
    ),
    (Value::Char(Some('é')), SqlValue::Text("é".into())),
    (
      Value::Enum(OptionEnum::Some(Box::new(Enum {
        type_name: enum_ty.clone(),
        value: "happy".into(),
      }))),
      SqlValue::Text("happy".into()),
    ),
    (
      Value::Bytes(Some(vec![0, 255])),
      SqlValue::Bytes(vec![0, 255]),
    ),
    (
      Value::Json(Some(Box::new(json.clone()))),
      SqlValue::Json(json),
    ),
    (
      Value::Uuid(Some(Uuid::from_u128(7))),
      SqlValue::Uuid(Uuid::from_u128(7)),
    ),
    (
      Value::Decimal(Some(Decimal::new(-12_345, 2))),
      SqlValue::Decimal(Decimal::new(-12_345, 2)),
    ),
    (
      Value::JiffTimestamp(Some(Box::new(t))),
      SqlValue::Timestamp(t),
    ),
    (
      Value::JiffDateTime(Some(Box::new(dt))),
      SqlValue::DateTime(dt),
    ),
    (
      Value::JiffDate(Some(date(-9999, 1, 1))),
      SqlValue::Date(date(-9999, 1, 1)),
    ),
    (
      Value::JiffTime(Some(clock(23, 59, 59, 999_999_999))),
      SqlValue::Time(clock(23, 59, 59, 999_999_999)),
    ),
    // time: the instant is kept, the +05:30 offset dropped.
    (
      Value::TimeDateTimeWithTimeZone(Some(odt)),
      SqlValue::Timestamp(t),
    ),
    (Value::TimeDateTime(Some(pdt)), SqlValue::DateTime(dt)),
    (
      Value::TimeDate(Some(pdt.date())),
      SqlValue::Date(date(2024, 2, 29)),
    ),
    (
      Value::TimeTime(Some(pdt.time())),
      SqlValue::Time(clock(12, 34, 56, 789_012_000)),
    ),
    (Value::Enum(OptionEnum::None(enum_ty)), SqlValue::Null),
  ]
}

#[test]
fn every_sea_variant_bridges_to_its_sql_value() {
  for (sea, sql) in table() {
    assert_eq!(SqlValue::try_from(sea.clone()), Ok(sql), "{sea:?}");
  }
}

#[test]
fn a_typed_null_of_every_variant_is_null() {
  for (sea, _) in table() {
    let null = sea.as_null();
    assert_eq!(
      SqlValue::try_from(null.clone()),
      Ok(SqlValue::Null),
      "{null:?}"
    );
  }
}

#[test]
fn sql_values_round_trip_through_sea_values() {
  for (_, sql) in table()
    .into_iter()
    .chain([(Value::Int(None), SqlValue::Null)])
  {
    let sea = Value::from(sql.clone());
    assert_eq!(SqlValue::try_from(sea), Ok(sql));
  }
  // Dates and times go in as jiff, NULL as an untyped (text) NULL.
  assert!(matches!(
    Value::from(SqlValue::Timestamp(ts("2000-01-01T00:00:00Z"))),
    Value::JiffTimestamp(Some(_))
  ));
  assert_eq!(Value::from(SqlValue::Null), Value::String(None));
}

#[test]
fn unsigned_beyond_i64_is_out_of_range_naming_the_parameter() {
  assert_eq!(
    SqlValue::try_from(Value::BigUnsigned(Some(u64::MAX))),
    Err(SqlError::ValueOutOfRange {
      column: "value".into(),
      message: "18446744073709551615 does not fit an I64".into(),
    })
  );
  let values = Values(vec![
    Value::Int(Some(1)),
    Value::BigUnsigned(Some(u64::MAX)),
  ]);
  assert!(matches!(
    sql_values(values),
    Err(SqlError::ValueOutOfRange { column, .. }) if column == "$2"
  ));
  assert_eq!(
    sql_values(Values(vec![Value::Int(Some(1)), Value::Bool(None)])),
    Ok(vec![SqlValue::I32(1), SqlValue::Null])
  );
}

#[test]
fn time_instants_beyond_jiff_are_out_of_range() {
  // time reaches 9999-12-31T23:59:59 UTC; jiff's Timestamp stops a day earlier.
  let last = time::PrimitiveDateTime::new(
    time::Date::from_calendar_date(9999, time::Month::December, 31).unwrap(),
    time::Time::from_hms(23, 59, 59).unwrap(),
  );
  assert!(matches!(
    SqlValue::try_from(Value::TimeDateTimeWithTimeZone(Some(last.assume_utc()))),
    Err(SqlError::ValueOutOfRange { column, .. }) if column == "value"
  ));
  // The wall clock at the same moment fits.
  assert_eq!(
    SqlValue::try_from(Value::TimeDateTime(Some(last))),
    Ok(SqlValue::DateTime(date(9999, 12, 31).at(23, 59, 59, 0)))
  );
}

fn crud() -> [Box<dyn BuildSql>; 4] {
  [
    Box::new(
      Query::select()
        .columns(["id", "title"])
        .from("todos")
        .and_where(Expr::col("done").eq(false))
        .and_where(Expr::col("owner").is_in([Uuid::from_u128(1)]))
        .order_by("id", Order::Desc)
        .limit(5)
        .to_owned(),
    ),
    Box::new(
      Query::insert()
        .into_table("todos")
        .columns(["title", "due"])
        .values_panic(["docs".into(), date(2024, 2, 29).into()])
        .on_conflict(OnConflict::column("title").do_nothing().to_owned())
        .to_owned(),
    ),
    Box::new(
      Query::update()
        .table("todos")
        .value("done", true)
        .value("note", SqlValue::Null)
        .and_where(Expr::col("id").eq(7))
        .to_owned(),
    ),
    Box::new(
      Query::delete()
        .from_table(Alias::new("todos"))
        .and_where(Expr::col("id").eq(7i64))
        .to_owned(),
    ),
  ]
}

#[test]
fn crud_statements_render_per_dialect() {
  let [select, insert, update, delete] = crud();
  let pg = |s: &dyn BuildSql| s.build_sql(Dialect::Postgres).unwrap();
  assert_eq!(
    pg(&*select),
    (
      r#"SELECT "id", "title" FROM "todos" WHERE "done" = $1 AND "owner" IN ($2) ORDER BY "id" DESC LIMIT $3"#
        .into(),
      vec![
        SqlValue::Bool(false),
        SqlValue::Uuid(Uuid::from_u128(1)),
        SqlValue::I64(5)
      ]
    )
  );
  assert_eq!(
    pg(&*insert),
    (
      r#"INSERT INTO "todos" ("title", "due") VALUES ($1, $2) ON CONFLICT ("title") DO NOTHING"#
        .into(),
      vec![
        SqlValue::Text("docs".into()),
        SqlValue::Date(date(2024, 2, 29))
      ]
    )
  );
  assert_eq!(
    pg(&*update),
    (
      r#"UPDATE "todos" SET "done" = $1, "note" = $2 WHERE "id" = $3"#.into(),
      vec![SqlValue::Bool(true), SqlValue::Null, SqlValue::I32(7)]
    )
  );
  assert_eq!(
    pg(&*delete),
    (
      r#"DELETE FROM "todos" WHERE "id" = $1"#.into(),
      vec![SqlValue::I64(7)]
    )
  );

  let sql = |s: &dyn BuildSql, d| s.build_sql(d).unwrap().0;
  assert_eq!(
    sql(&*update, Dialect::MySql),
    "UPDATE `todos` SET `done` = ?, `note` = ? WHERE `id` = ?"
  );
  assert_eq!(
    sql(&*update, Dialect::Sqlite),
    r#"UPDATE "todos" SET "done" = ?, "note" = ? WHERE "id" = ?"#
  );
  assert_eq!(
    sql(&*delete, Dialect::MySql),
    "DELETE FROM `todos` WHERE `id` = ?"
  );
  assert_eq!(
    sql(&*select, Dialect::Sqlite),
    r#"SELECT "id", "title" FROM "todos" WHERE "done" = ? AND "owner" IN (?) ORDER BY "id" DESC LIMIT ?"#
  );
  // (sea-query's MySQL `do_nothing` is `ON DUPLICATE KEY IGNORE`; MySQL wants `do_nothing_on`.)
  assert!(
    sql(&*insert, Dialect::MySql).starts_with("INSERT INTO `todos` (`title`, `due`) VALUES (?, ?)")
  );
  // The parameters do not depend on the dialect.
  for s in crud() {
    let params = s.build_sql(Dialect::Postgres).unwrap().1;
    for d in [Dialect::MySql, Dialect::Sqlite] {
      assert_eq!(s.build_sql(d).unwrap().1, params);
    }
  }
}

#[test]
fn joins_filters_ordering_and_paging_render_with_parameters() {
  let page = Query::select()
    .column(("t", "id"))
    .column(("u", "name"))
    .from_as("todos", "t")
    .join_as(
      JoinType::InnerJoin,
      "users",
      "u",
      Expr::col(("u", "id")).equals(("t", "owner")),
    )
    .and_where(Expr::col(("t", "title")).like("%docs%"))
    .and_where(Expr::col(("u", "id")).is_not_null())
    .order_by(("t", "id"), Order::Asc)
    .limit(20)
    .offset(40)
    .to_owned();
  assert_eq!(
    page.build_sql(Dialect::Postgres).unwrap(),
    (
      r#"SELECT "t"."id", "u"."name" FROM "todos" AS "t" INNER JOIN "users" AS "u" ON "u"."id" = "t"."owner" WHERE "t"."title" LIKE $1 AND "u"."id" IS NOT NULL ORDER BY "t"."id" ASC LIMIT $2 OFFSET $3"#
        .into(),
      vec![
        SqlValue::Text("%docs%".into()),
        SqlValue::I64(20),
        SqlValue::I64(40)
      ]
    )
  );
}

fn run<A: Send + 'static>(
  test: &TestSqlClient,
  program: Effect<A, SqlError, SqlClientEnv>,
) -> Exit<A, SqlError> {
  program
    .provide_layer(sql_client_layer::<SqlError>(test.clone()))
    .block_on()
}

#[test]
fn helpers_run_the_rendered_statement() {
  let [select, insert, update, _] = crud();
  let (select_sql, select_params) = select.build_sql(Dialect::Postgres).unwrap();
  let (update_sql, _) = update.build_sql(Dialect::Postgres).unwrap();
  let test = TestSqlClient::new();
  test.script_query(
    select_sql.clone(),
    vec![
      SqlRow::from_pairs([("id", SqlValue::I64(2)), ("title", "b".into())]),
      SqlRow::from_pairs([("id", SqlValue::I64(1)), ("title", "a".into())]),
    ],
  );
  test.script_execute(update_sql.clone(), 3);

  let all = run(
    &test,
    fetch_all_as::<(i64, String), _, _>(Dialect::Postgres, &*select),
  );
  assert!(matches!(all, Exit::Success(ref v) if v == &[(2, "b".into()), (1, "a".into())]));
  let one = run(
    &test,
    fetch_one_as::<(i64,), _, _>(Dialect::Postgres, &*select),
  );
  assert!(matches!(one, Exit::Success((2,))));
  let opt = run(
    &test,
    fetch_optional_as::<SqlRow, _, _>(Dialect::Postgres, &*select),
  );
  assert!(matches!(opt, Exit::Success(Some(ref r)) if r.get::<i64>("id") == Ok(2)));
  assert!(matches!(
    run(&test, execute_query(Dialect::Postgres, &*update)),
    Exit::Success(3)
  ));
  assert_eq!(test.recorded_queries()[0], (select_sql, select_params));
  assert_eq!(test.recorded_executes()[0].0, update_sql);

  // No rows: `None` / `RowNotFound`.
  let none = run(
    &test,
    fetch_optional_as::<(i64,), _, _>(Dialect::Postgres, &*insert),
  );
  assert!(matches!(none, Exit::Success(None)));
  let missing = run(
    &test,
    fetch_one_as::<(i64,), _, _>(Dialect::Postgres, &*insert),
  );
  assert!(matches!(
    missing,
    Exit::Failure(Cause::Fail(SqlError::RowNotFound))
  ));
}

#[test]
fn a_rendering_error_fails_the_effect_before_any_query() {
  let bad = Query::select()
    .expr(Expr::val(u64::MAX))
    .and_where(Expr::col("id").eq(1))
    .to_owned();
  assert!(matches!(
    bad.build_sql(Dialect::Sqlite),
    Err(SqlError::ValueOutOfRange { ref column, .. }) if column == "$1"
  ));
  let test = TestSqlClient::new();
  let failed = |exit: Exit<(), SqlError>| {
    matches!(
      exit,
      Exit::Failure(Cause::Fail(SqlError::ValueOutOfRange { .. }))
    )
  };
  let pg = Dialect::Postgres;
  assert!(failed(run(
    &test,
    fetch_all_as::<SqlRow, _, _>(pg, &bad).map(|_| ())
  )));
  assert!(failed(run(
    &test,
    fetch_one_as::<SqlRow, _, _>(pg, &bad).map(|_| ())
  )));
  assert!(failed(run(
    &test,
    fetch_optional_as::<SqlRow, _, _>(pg, &bad).map(|_| ())
  )));
  assert!(failed(run(&test, execute_query(pg, &bad).map(|_| ()))));
  assert!(test.recorded_queries().is_empty() && test.recorded_executes().is_empty());
}

proptest! {
  /// sea-query's `time` values reach the same jiff values as its jiff ones.
  #[test]
  fn prop_time_values_bridge_like_jiff_values(
    secs in -62_135_596_800i64..253_402_200_000,
    nanos in 0i32..1_000_000_000,
    offset in -86_399i32..86_400,
  ) {
    let t = jiff::Timestamp::new(secs, nanos).unwrap();
    let odt = utc(t).to_offset(time::UtcOffset::from_whole_seconds(offset).unwrap());
    prop_assert_eq!(
      SqlValue::try_from(Value::TimeDateTimeWithTimeZone(Some(odt))),
      SqlValue::try_from(Value::JiffTimestamp(Some(Box::new(t))))
    );
    let civil = jiff::tz::Offset::UTC.to_datetime(t);
    let pdt = time::PrimitiveDateTime::new(utc(t).date(), utc(t).time());
    prop_assert_eq!(
      SqlValue::try_from(Value::TimeDateTime(Some(pdt))),
      Ok(SqlValue::DateTime(civil))
    );
    prop_assert_eq!(
      SqlValue::try_from(Value::TimeDate(Some(pdt.date()))),
      Ok(SqlValue::Date(civil.date()))
    );
    prop_assert_eq!(
      SqlValue::try_from(Value::TimeTime(Some(pdt.time()))),
      Ok(SqlValue::Time(civil.time()))
    );
  }
}

#[cfg(feature = "proptest")]
proptest! {
  /// `SqlValue → sea_query::Value → SqlValue` is the identity on the portable domain.
  #[test]
  fn prop_sql_values_round_trip_through_sea_query(v in crate::testing::arb_sql_value()) {
    prop_assert_eq!(SqlValue::try_from(Value::from(v.clone())), Ok(v));
  }
}
