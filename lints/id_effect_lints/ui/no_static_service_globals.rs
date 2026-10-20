// E-02: a service in a `static`.
#![allow(dead_code)]

struct Database;

static DB: Database = Database;

// Primitive statics (and references to them) are fine.
static RETRIES: u32 = 3;
static NAME: &str = "orders";

fn main() {}
