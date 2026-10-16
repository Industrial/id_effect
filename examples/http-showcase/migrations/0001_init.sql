-- 0001_init: the todo list. Titles are unique: a duplicate answers 409 Conflict.
-- Applied at startup (`Migration::apply`, when `PgTodoRepositoryLive` is built) inside one
-- transaction holding an advisory lock, so
-- concurrent starts (and parallel tests) do not race on the DDL. Every statement is idempotent.
CREATE TABLE IF NOT EXISTS todos (
  id BIGSERIAL PRIMARY KEY,
  title TEXT NOT NULL UNIQUE,
  completed BOOLEAN NOT NULL DEFAULT FALSE
);
