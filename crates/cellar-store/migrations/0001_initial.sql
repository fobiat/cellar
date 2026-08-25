-- Cellar's schema. Two halves that share a database and nothing else.
--
-- `aj_*` is the bridge: the documents AppleJackRP's HostedDocumentStore reads
-- and writes over `/v1/doc/{key}`. `srv_*` is Cellar's own operations record.
-- They are kept apart by prefix on purpose: one is the gamemode's data and
-- outlives Cellar, the other is Cellar's observations and does not.

-- ---------------------------------------------------------------------------
-- The bridge
-- ---------------------------------------------------------------------------

-- `doc_key` is 128 characters because that is `DocumentKeys.MaximumLength`, and
-- the charset there is [a-z0-9._-/], so the key is safe as a primary key and in
-- a URL path with no escaping anywhere.
--
-- `scope` answers 20_PERSISTENCE.md's open question Q1 as "one server's data,
-- off-box", the simple reading. The column exists so that the other reading is
-- a migration rather than a rewrite.
CREATE TABLE aj_document (
    scope       VARCHAR(64)  NOT NULL,
    doc_key     VARCHAR(128) NOT NULL,
    body        JSON         NOT NULL,
    revision    BIGINT UNSIGNED NOT NULL DEFAULT 1,
    created_at  TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at  TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
    updated_by  VARCHAR(128) NULL,
    PRIMARY KEY (scope, doc_key)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

-- Every write, kept. A character profile holds a balance and an inventory, and
-- "who overwrote my character, and with what" is unanswerable without this.
-- It is also what makes a bad write recoverable rather than final.
CREATE TABLE aj_document_revision (
    id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    scope       VARCHAR(64)  NOT NULL,
    doc_key     VARCHAR(128) NOT NULL,
    revision    BIGINT UNSIGNED NOT NULL,
    body        JSON         NOT NULL,
    written_at  TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    written_by  VARCHAR(128) NULL,
    PRIMARY KEY (id),
    KEY idx_document_revision (scope, doc_key, revision)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

-- ---------------------------------------------------------------------------
-- Operations
-- ---------------------------------------------------------------------------

CREATE TABLE srv_session (
    id           BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    scope        VARCHAR(64)  NOT NULL,
    host         VARCHAR(255) NULL,
    command      TEXT         NULL,
    started_at   TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    ready_at     TIMESTAMP(3) NULL,
    ended_at     TIMESTAMP(3) NULL,
    exit_code    INT          NULL,
    graceful     BOOLEAN      NOT NULL DEFAULT FALSE,
    PRIMARY KEY (id),
    KEY idx_session_scope_started (scope, started_at)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

-- One row per account ever seen, so "who plays here" survives every restart.
CREATE TABLE srv_player (
    steam_id      BIGINT UNSIGNED NOT NULL,
    last_name     VARCHAR(255) NOT NULL,
    first_seen    TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    last_seen     TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    total_seconds BIGINT UNSIGNED NOT NULL DEFAULT 0,
    sessions      BIGINT UNSIGNED NOT NULL DEFAULT 0,
    PRIMARY KEY (steam_id)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE srv_player_session (
    id           BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    session_id   BIGINT UNSIGNED NULL,
    steam_id     BIGINT UNSIGNED NOT NULL,
    name         VARCHAR(255) NOT NULL,
    joined_at    TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    left_at      TIMESTAMP(3) NULL,
    leave_reason VARCHAR(32)  NULL,
    PRIMARY KEY (id),
    KEY idx_player_session_steam (steam_id, joined_at),
    KEY idx_player_session_open (session_id, left_at)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

CREATE TABLE srv_event (
    id         BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    session_id BIGINT UNSIGNED NULL,
    at         TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    kind       VARCHAR(32)  NOT NULL,
    logger     VARCHAR(64)  NULL,
    steam_id   BIGINT UNSIGNED NULL,
    payload    JSON         NULL,
    PRIMARY KEY (id),
    KEY idx_event_at (at),
    KEY idx_event_kind_at (kind, at)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;

-- The console audit. Every command the web UI or the CLI sent, who sent it, and
-- what came back. The console runs at full engine privilege, so this is the only
-- record of who used it.
CREATE TABLE srv_command (
    id         BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
    session_id BIGINT UNSIGNED NULL,
    at         TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    actor      VARCHAR(64)  NOT NULL,
    command    TEXT         NOT NULL,
    reply      MEDIUMTEXT   NULL,
    ok         BOOLEAN      NOT NULL DEFAULT TRUE,
    PRIMARY KEY (id),
    KEY idx_command_at (at)
) ENGINE = InnoDB DEFAULT CHARSET = utf8mb4;
