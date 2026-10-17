//! Typed values and rows without a database: `params!` encodes Rust values as `SqlValue`s,
//! `SqlRow::get` decodes a column as a Rust type (errors name the column), and
//! `#[derive(FromRow)]` decodes a whole row into a struct — here through `query_as` over the
//! in-memory `TestSqlClient`.
//!
//! Run: `cargo run -p id_effect_sql --example 020_typed_rows`

use id_effect::Exit;
use id_effect_sql::jiff::civil::{Date, date};
use id_effect_sql::rust_decimal::Decimal;
use id_effect_sql::{
  FromRow, Json, SqlClientEnv, SqlError, SqlRow, SqlType, SqlValue, TestSqlClient, params,
  query_as, sql_client_layer,
};
use serde_json::json;

const OPEN_INVOICES: &str =
  "SELECT id, customer, total, due, paid, tags FROM invoices WHERE paid = $1 AND total >= $2";

/// One invoice row: each field is decoded with `FromSqlValue` (`Option` for a nullable column,
/// `Json<T>` for a JSON column decoded with serde), `rename` maps a column of another name.
#[derive(Debug, PartialEq, FromRow)]
struct Invoice {
  id: i64,
  #[from_row(rename = "customer")]
  customer_name: String,
  total: Decimal,
  due: Date,
  paid: Option<bool>,
  tags: Json<Vec<String>>,
}

fn invoice_row(id: i64, customer: &str, cents: i64) -> SqlRow {
  SqlRow::from_pairs([
    ("id", SqlValue::I64(id)),
    ("customer", customer.into()),
    ("total", SqlValue::Decimal(Decimal::new(cents, 2))),
    ("due", SqlValue::Date(date(2026, 11, 30))),
    ("paid", SqlValue::Null),
    ("tags", SqlValue::Json(json!(["eu", "net30"]))),
  ])
}

fn main() {
  // `params!` encodes each argument with `ToSqlValue`: the statement's typed parameters.
  let params = params![false, Decimal::new(10_000, 2)];
  assert_eq!(
    params,
    [
      SqlValue::Bool(false),
      SqlValue::Decimal(Decimal::new(10_000, 2))
    ]
  );
  assert_eq!(params[1].sql_type(), SqlType::Decimal);
  let none: Option<i64> = None;
  assert_eq!(
    params![none, "text", 7i32],
    [
      SqlValue::Null,
      SqlValue::Text("text".into()),
      SqlValue::I32(7)
    ]
  );

  // `SqlRow::get` decodes one column, by name or by position.
  let row = invoice_row(1, "acme", 12_500);
  assert_eq!(row.get::<i64>("id"), Ok(1));
  assert_eq!(row.get::<String>(1usize), Ok("acme".to_owned()));
  assert_eq!(row.get::<Option<bool>>("paid"), Ok(None));
  assert_eq!(row.get::<Decimal>("total"), Ok(Decimal::new(12_500, 2)));
  // A wrong type, a NULL in a non-optional column or a missing column: the error names it.
  assert_eq!(
    row.get::<i64>("customer"),
    Err(SqlError::TypeMismatch {
      column: "customer".into(),
      expected: SqlType::I64,
      found: SqlType::Text,
    })
  );
  assert_eq!(
    row.get::<bool>("paid"),
    Err(SqlError::UnexpectedNull {
      column: "paid".into()
    })
  );
  assert_eq!(
    row.get::<i64>("nope"),
    Err(SqlError::ColumnNotFound {
      column: "nope".into()
    })
  );

  // `#[derive(FromRow)]` decodes the whole row; tuples decode by position.
  let invoice = Invoice::from_row(&row).expect("an invoice");
  assert_eq!(invoice.customer_name, "acme");
  assert_eq!(invoice.tags.0, ["eu", "net30"]);
  assert_eq!(<(i64, String)>::from_row(&row), Ok((1, "acme".to_owned())));

  // The same decoding behind `query_as`, against the in-memory client: no database needed.
  let test = TestSqlClient::new();
  test.script_query(
    OPEN_INVOICES,
    vec![
      invoice_row(1, "acme", 12_500),
      invoice_row(2, "globex", 30_000),
    ],
  );
  let open = query_as::<Invoice, SqlClientEnv, _>(OPEN_INVOICES, params)
    .provide_layer(sql_client_layer::<SqlError>(test.clone()));
  let Exit::Success(invoices) = open.block_on() else {
    panic!("the scripted query fails")
  };
  assert_eq!(
    invoices.iter().map(|i| i.id).collect::<Vec<_>>(),
    [1, 2],
    "{invoices:?}"
  );
  // The client saw the typed parameters `params!` built.
  let (sql, sent) = &test.recorded_queries()[0];
  assert_eq!(
    (sql.as_str(), sent[0].clone()),
    (OPEN_INVOICES, SqlValue::Bool(false))
  );

  // A row that does not fit the struct fails the effect with the column's error.
  test.script_query(
    "SELECT id FROM invoices",
    vec![SqlRow::from_pairs([("id", 1i64)])],
  );
  let short = query_as::<Invoice, SqlClientEnv, _>("SELECT id FROM invoices", vec![])
    .provide_layer(sql_client_layer::<SqlError>(test));
  assert!(matches!(
    short.block_on(),
    Exit::Failure(id_effect::Cause::Fail(SqlError::ColumnNotFound { ref column })) if column == "customer"
  ));

  println!(
    "020_typed_rows ok ({} invoices, {} due {})",
    invoices.len(),
    invoices[0].customer_name,
    invoices[0].due
  );
}
