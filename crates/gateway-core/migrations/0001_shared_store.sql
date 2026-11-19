-- Shared Gateway control-plane store, schema version 1.
--
-- One database may serve several Gateway replicas. Mutations are one
-- transaction per store call. The dispatch table owns the caller-scoped request
-- identity and the stream mapping so they cannot drift apart.

CREATE TABLE IF NOT EXISTS gateway_schema (
  id      integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  version integer NOT NULL
);

INSERT INTO gateway_schema (id, version) VALUES (1, 1)
ON CONFLICT (id) DO NOTHING;

-- Registry state is one serialized document guarded by a row lock. The adapter
-- never keeps an authoritative copy in process memory. Each call loads,
-- mutates, and commits under `SELECT ... FOR UPDATE`.
CREATE TABLE IF NOT EXISTS gateway_registry (
  id       integer PRIMARY KEY DEFAULT 1 CHECK (id = 1),
  document jsonb   NOT NULL DEFAULT '{}'::jsonb,
  revision bigint  NOT NULL DEFAULT 0
);

INSERT INTO gateway_registry (id) VALUES (1) ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS gateway_dispatch (
  execution_id       text PRIMARY KEY,
  namespace          text NOT NULL,
  caller             text NOT NULL,
  idempotency_key    text NOT NULL,
  request_digest     text NOT NULL,
  worker             text NOT NULL,
  agent              text NOT NULL,
  state              text NOT NULL
                     CHECK (state IN ('intent','dispatching','accepted','unknown')),
  worker_instance    text NOT NULL DEFAULT '',
  worker_incarnation text NOT NULL DEFAULT '',
  worker_task_id     text,
  accepted           jsonb,
  projection         jsonb,
  connectivity       text NOT NULL DEFAULT 'unknown'
                     CHECK (connectivity IN ('unknown','online','unreachable')),
  last_error         text,
  revision           bigint NOT NULL DEFAULT 0,
  claim_id           text,
  claimed_by         text,
  claim_expires_at   timestamptz,
  created_at         timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at         timestamptz NOT NULL DEFAULT clock_timestamp(),
  transitions        jsonb NOT NULL DEFAULT '[]'::jsonb,
  CONSTRAINT dispatch_identity_uq UNIQUE (namespace, caller, idempotency_key),
  CONSTRAINT dispatch_accept_consistent
    CHECK (state <> 'accepted' OR worker_task_id IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS dispatch_claim_expiry
  ON gateway_dispatch (claim_expires_at)
  WHERE state = 'dispatching';

CREATE TABLE IF NOT EXISTS gateway_audit (
  id        bigserial PRIMARY KEY,
  at        timestamptz NOT NULL DEFAULT clock_timestamp(),
  action    text NOT NULL,
  worker    text NOT NULL,
  principal text NOT NULL,
  outcome   text NOT NULL,
  detail    text NOT NULL
);

CREATE INDEX IF NOT EXISTS gateway_audit_worker
  ON gateway_audit (worker, id DESC);
