use rusqlite::Connection;

use crate::{Error, Result};

const MIGRATIONS: &[&str] = &[r"
CREATE TABLE repos (
    id INTEGER PRIMARY KEY,
    installation_id INTEGER NOT NULL,
    owner TEXT NOT NULL,
    name TEXT NOT NULL,
    default_branch TEXT NOT NULL,
    private INTEGER NOT NULL,
    archived INTEGER NOT NULL,
    enrolled INTEGER NOT NULL DEFAULT 0,
    paused INTEGER NOT NULL DEFAULT 0,
    etag_head TEXT,
    etag_tags TEXT,
    etag_pulls TEXT,
    head_sha TEXT,
    rc_high_water INTEGER NOT NULL DEFAULT 0,
    config_error TEXT,
    last_seen_at INTEGER,
    removed_at INTEGER,
    UNIQUE (owner, name)
);

CREATE TABLE seen_tags (
    repo_id INTEGER NOT NULL REFERENCES repos (id),
    name TEXT NOT NULL,
    PRIMARY KEY (repo_id, name)
);

CREATE TABLE builds (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL REFERENCES repos (id),
    number INTEGER NOT NULL,
    trigger TEXT NOT NULL,
    ref_name TEXT NOT NULL,
    pr_number INTEGER,
    sha TEXT NOT NULL,
    message TEXT,
    dedupe_key TEXT NOT NULL UNIQUE,
    actor TEXT,
    trusted INTEGER NOT NULL,
    config TEXT,
    state TEXT NOT NULL,
    phase TEXT,
    failure TEXT,
    detail TEXT,
    attempt INTEGER NOT NULL DEFAULT 1,
    not_before INTEGER NOT NULL DEFAULT 0,
    pod TEXT,
    version TEXT,
    rerun_of INTEGER REFERENCES builds (id),
    cancel_requested INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    UNIQUE (repo_id, number)
);
CREATE INDEX builds_state ON builds (state);
CREATE INDEX builds_repo ON builds (repo_id, id);
CREATE INDEX builds_sha ON builds (repo_id, sha);

CREATE TABLE tasks (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    attempt INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    container TEXT NOT NULL,
    state TEXT NOT NULL,
    exit_code INTEGER,
    reason TEXT,
    started_at INTEGER,
    finished_at INTEGER,
    log_final INTEGER NOT NULL DEFAULT 0,
    log_bytes INTEGER,
    PRIMARY KEY (build_id, attempt, ordinal)
);

CREATE TABLE steps (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    attempt INTEGER NOT NULL,
    task INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL,
    exit_code INTEGER,
    started_at INTEGER,
    finished_at INTEGER,
    log_offset INTEGER,
    PRIMARY KEY (build_id, attempt, task, ordinal)
);

CREATE TABLE images (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    ordinal INTEGER NOT NULL,
    name TEXT NOT NULL,
    digest TEXT,
    released TEXT,
    PRIMARY KEY (build_id, ordinal)
);

CREATE TABLE checks (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    name TEXT NOT NULL,
    check_id INTEGER NOT NULL,
    PRIMARY KEY (build_id, name)
);

CREATE TABLE outbox (
    id INTEGER PRIMARY KEY,
    build_id INTEGER NOT NULL REFERENCES builds (id),
    kind TEXT NOT NULL,
    payload TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    next_at INTEGER NOT NULL,
    last_error TEXT,
    done_at INTEGER
);
CREATE INDEX outbox_due ON outbox (done_at, next_at);

CREATE TABLE sessions (
    id_hash TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL,
    login TEXT NOT NULL,
    avatar_url TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    seen_at INTEGER NOT NULL,
    ip TEXT,
    agent TEXT
);

CREATE TABLE audit (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    repo_id INTEGER,
    build_id INTEGER,
    detail TEXT
);
"];

pub fn migrate(conn: &Connection) -> Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let known = MIGRATIONS.len() as i64;
    if current > known {
        return Err(Error::TooNew { found: current, known });
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        conn.execute_batch(&format!("BEGIN; {migration} PRAGMA user_version = {}; COMMIT;", index + 1))?;
    }
    Ok(())
}
