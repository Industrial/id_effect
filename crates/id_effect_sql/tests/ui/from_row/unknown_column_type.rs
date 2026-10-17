use id_effect_sql::FromRow;

struct Money(i64);

#[derive(FromRow)]
struct Invoice {
  id: i64,
  total: Money,
}

fn main() {}
