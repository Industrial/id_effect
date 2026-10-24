-- The Pipeline's data model (release spec 0.13.0, D1): `sources` 1:N `documents` N:M `labels`
-- (through `document_labels`), `documents` 1:N `extractions`, and one embedding per document.
-- Every row is tenant-scoped (`tenant`): a tenant on the shared main database is separated by
-- this column, a tenant with a database of its own (`TenantRouter`) by both.

CREATE TABLE IF NOT EXISTS sources (
  id     bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant text   NOT NULL,
  name   text   NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
  url    text   NOT NULL,
  trust  text   NOT NULL DEFAULT 'unverified' CHECK (trust IN ('unverified', 'verified')),
  UNIQUE (tenant, name)
);

CREATE TABLE IF NOT EXISTS labels (
  id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant      text   NOT NULL,
  name        text   NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
  description text   NOT NULL,
  UNIQUE (tenant, name)
);

CREATE TABLE IF NOT EXISTS documents (
  id           bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant       text        NOT NULL,
  source_id    bigint      NOT NULL REFERENCES sources (id),
  title        text        NOT NULL CHECK (length(title) BETWEEN 1 AND 200),
  body         text        NOT NULL,
  status       text        NOT NULL CHECK (status IN
                 ('ingested', 'extracted', 'classified', 'review', 'published', 'failed')),
  duplicate_of bigint      REFERENCES documents (id),
  version      bigint      NOT NULL DEFAULT 1,
  created_at   timestamptz NOT NULL DEFAULT now(),
  deleted_at   timestamptz
);

-- Keyset pages and the near-duplicate candidates: newest documents of one tenant first.
CREATE INDEX IF NOT EXISTS documents_tenant_id_idx ON documents (tenant, id);

CREATE TABLE IF NOT EXISTS document_labels (
  document_id bigint NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
  label_id    bigint NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
  PRIMARY KEY (document_id, label_id)
);

CREATE TABLE IF NOT EXISTS extractions (
  id           bigint      GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  tenant       text        NOT NULL,
  document_id  bigint      NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
  title        text        NOT NULL,
  summary      text        NOT NULL,
  language     text        NOT NULL CHECK (language IN ('en', 'de')),
  organization text,
  -- What the MCP enrichment server said about `organization` (JSON text), if it was asked.
  enrichment   text,
  model        text        NOT NULL,
  created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS extractions_document_idx ON extractions (document_id);

-- Embeddings are compared in the application over a bounded candidate set (no vector index:
-- release spec 0.13.0 non-goal).
CREATE TABLE IF NOT EXISTS document_embeddings (
  document_id bigint NOT NULL PRIMARY KEY REFERENCES documents (id) ON DELETE CASCADE,
  tenant      text   NOT NULL,
  vector      jsonb  NOT NULL
);

CREATE INDEX IF NOT EXISTS document_embeddings_tenant_idx
  ON document_embeddings (tenant, document_id DESC);
