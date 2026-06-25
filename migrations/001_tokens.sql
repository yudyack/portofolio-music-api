-- music-api v1 schema. Single-row tokens table — see spec §5.10.
-- One owner only. CHECK (id = 1) enforces the invariant. If multi-tenant is
-- ever in scope, drop the CHECK and add a unique index on owner_id.
CREATE TABLE tokens (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    access_token  TEXT NOT NULL,
    refresh_token TEXT NOT NULL,
    expires_at    TEXT NOT NULL, -- ISO8601 UTC
    scope         TEXT NOT NULL,
    owner_id      TEXT NOT NULL,
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
