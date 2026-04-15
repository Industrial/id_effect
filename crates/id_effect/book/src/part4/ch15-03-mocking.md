# Mocking Services — Test Doubles via Layers

In id_effect, "mocking" isn't a special testing concept — it's just providing a different `Layer`. Production code gets a `PostgresDb` layer. Test code gets an `InMemoryDb` layer. Business logic never knows the difference.

No mock frameworks. No `#[automock]`. No `vi.mock()` equivalent. Just layers.

## The Pattern

Define a service trait (or use a service key with a trait object):

```rust
service_key!(DbKey: Arc<dyn Db>);

trait Db: Send + Sync {
    fn get_user(&self, id: UserId) -> Effect<User, DbError, ()>;
    fn save_user(&self, user: User) -> Effect<(), DbError, ()>;
}
```

Provide two implementations — one for production, one for tests:

```rust
// Production
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
```

## Injecting the Test Double

```rust
#[test]
fn get_user_returns_saved_user() {
    let db = Arc::new(InMemoryDb::new());
    let env = ctx!(DbKey => db.clone() as Arc<dyn Db>);

    let eff = effect!(|r: &mut Deps| {
        let db = ~ DbKey;
        ~ db.save_user(User { id: UserId::new(1), name: "Alice".into() });
        ~ db.get_user(UserId::new(1))
    });

    let exit = run_test_with_env(eff, env);
    let user = exit.unwrap_success();
    assert_eq!(user.name, "Alice");
}
```

The business logic (`save_user` then `get_user`) is identical to production. Only the environment differs.

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

#[test]
fn registration_sends_welcome_email() {
    let spy = Arc::new(SpyMailer::new());
    let env = ctx!(MailerKey => spy.clone() as Arc<dyn Mailer>);

    let exit = run_test_with_env(register_user("alice@example.com"), env);
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

#[test]
fn get_user_propagates_db_errors() {
    let env = ctx!(DbKey => Arc::new(FailingDb) as Arc<dyn Db>);
    let exit = run_test_with_env(get_user(UserId::new(1)), env);
    assert!(matches!(exit, Exit::Failure(Cause::Fail(DbError::ConnectionLost))));
}
```

## Layer-Based Test Setup

For more complex scenarios, build a test layer:

```rust
fn test_layer() -> Layer<Deps, (), ()> {
    Layer::provide(DbKey, Arc::new(InMemoryDb::new()) as Arc<dyn Db>)
        .stack(Layer::provide(MailerKey, Arc::new(SpyMailer::new()) as Arc<dyn Mailer>))
        .stack(Layer::provide(ClockKey, Arc::new(TestClock::new()) as Arc<dyn Clock>))
}

#[test]
fn full_registration_flow() {
    let env = test_layer().build().unwrap();
    let exit = run_test_with_env(full_registration_flow(), env);
    assert!(matches!(exit, Exit::Success(_)));
}
```

The test layer mirrors your production layer in structure but with test implementations. Add new services in one place and all tests pick them up.

## What You Don't Need

- No `mockall`, no `mock!` macros
- No `#[cfg(test)]` on business logic
- No `Box<dyn Fn(…)>` callback injection patterns
- No global state reset between tests

The `Layer` system is the mock framework.
