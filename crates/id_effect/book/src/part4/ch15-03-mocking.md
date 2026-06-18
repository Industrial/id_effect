# Mocking Services — Test Doubles via Providers

In id_effect, "mocking" isn't a special testing concept — it's just providing a different `ProviderSpec`. Production code gets `PostgresDbLive`. Test code gets `InMemoryDbMock`. Business logic never knows the difference.

No mock frameworks. No `#[automock]`. No `vi.mock()` equivalent. Just providers.

## The Pattern

Declare a capability key and a trait object (or concrete type):

```rust
#[::id_effect::capability(Arc<dyn Db>)]
struct Database;

trait Db: Send + Sync {
    fn get_user(&self, id: UserId) -> Effect<User, DbError, ()>;
    fn save_user(&self, user: User) -> Effect<(), DbError, ()>;
}
```

Provide two implementations — one for production, one for tests:

```rust
// Production
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(DatabaseKey)]
struct PostgresDbLive;

struct PostgresDb { pool: PgPool }
impl Db for PostgresDb { /* real SQL queries */ }

// Test double
struct InMemoryDb { users: Mutex<HashMap<UserId, User>> }
impl Db for InMemoryDb {
    fn get_user(&self, id: UserId) -> Effect<User, DbError, ()> {
        let users = self.users.lock().unwrap();
        match users.get(&id) {
            Some(u) => succeed(u.clone()),
            None    => fail(DbError::NotFound(id)),
        }
    }

    fn save_user(&self, user: User) -> Effect<(), DbError, ()> {
        self.users.lock().unwrap().insert(user.id, user);
        succeed(())
    }
}

mock_capability!(InMemoryDbMock, DatabaseKey, Arc<dyn Db>, "db/inmemory", || {
    Arc::new(InMemoryDb::new()) as Arc<dyn Db>
});
```

## Injecting the Test Double

```rust
#[test]
fn get_user_returns_saved_user() {
    let env = build_env([provide!(InMemoryDbMock)]).expect("env");

    let eff = effect!(|r| {
        let db = ~DatabaseKey;
        ~ db.save_user(User { id: UserId::new(1), name: "Alice".into() });
        ~ db.get_user(UserId::new(1))
    });

    let exit = run_test(eff, env);
    let Exit::Success(user) = exit else { panic!("expected success") };
    assert_eq!(user.name, "Alice");
}
```

The business logic (`save_user` then `get_user`) is identical to production. Only the provider list differs.

## Asserting on Calls

When you need to verify that a service was called with specific arguments, add tracking to the test double:

```rust
struct SpyMailer {
    sent: Mutex<Vec<Email>>,
}

impl Mailer for SpyMailer {
    fn send(&self, email: Email) -> Effect<(), MailError, ()> {
        self.sent.lock().unwrap().push(email.clone());
        succeed(())
    }
}

#[::id_effect::capability(Arc<dyn Mailer>)]
struct MailerCap;

mock_capability!(SpyMailerMock, MailerCapKey, Arc<dyn Mailer>, "mailer/spy", || {
    Arc::new(SpyMailer::new()) as Arc<dyn Mailer>
});

#[test]
fn registration_sends_welcome_email() {
    let env = build_env([provide!(SpyMailerMock)]).expect("env");
    let spy = env.get::<MailerCapKey>().clone();

    let exit = run_test(register_user("alice@example.com"), env);
    assert!(matches!(exit, Exit::Success(_)));

    let sent = spy.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "alice@example.com");
}
```

## Failing Services

Test that your code handles service failures correctly by providing a failing test double:

```rust
struct FailingDb;
impl Db for FailingDb {
    fn get_user(&self, _id: UserId) -> Effect<User, DbError, ()> {
        fail(DbError::ConnectionLost)
    }
    fn save_user(&self, _user: User) -> Effect<(), DbError, ()> {
        fail(DbError::ConnectionLost)
    }
}

mock_capability!(FailingDbMock, DatabaseKey, Arc<dyn Db>, "db/failing", || {
    Arc::new(FailingDb) as Arc<dyn Db>
});

#[test]
fn get_user_propagates_db_errors() {
    let env = build_env([provide!(FailingDbMock)]).expect("env");
    let exit = run_test(get_user(UserId::new(1)), env);
    assert!(matches!(exit, Exit::Failure(Cause::Fail(DbError::ConnectionLost))));
}
```

## Provider-Based Test Setup

For more complex scenarios, build a shared test env:

```rust
fn test_env() -> Env {
    build_env([
        provide!(InMemoryDbMock),
        provide!(SpyMailerMock),
        provide!(TestClockLive),
    ])
    .expect("test env")
}

#[test]
fn full_registration_flow() {
    let env = test_env();
    let exit = run_test(full_registration_flow(), env);
    assert!(matches!(exit, Exit::Success(_)));
}
```

The test provider list mirrors your production wiring in structure but with test implementations. Add new providers in one place and all tests pick them up.

## What You Don't Need

- No `mockall`, no `mock!` macros
- No `#[cfg(test)]` on business logic
- No `Box<dyn Fn(…)>` callback injection patterns
- No global state reset between tests

The capability provider graph is the mock framework.
