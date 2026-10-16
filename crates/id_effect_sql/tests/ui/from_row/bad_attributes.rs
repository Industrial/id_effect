use id_effect_sql::FromRow;

#[derive(FromRow)]
struct Unknown {
  #[from_row(skip)]
  id: i64,
}

#[derive(FromRow)]
#[from_row(schema)]
struct SchemaRename {
  #[from_row(rename = "user_id")]
  id: i64,
}

fn main() {}
