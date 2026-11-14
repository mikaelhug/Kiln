mod model;
mod schema;

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, params};
use thiserror::Error;

pub use model::*;

#[derive(Debug, Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the database has schema version {found}, newer than this Kiln understands ({known})")]
    TooNew { found: i64, known: i64 },
}

pub type Result<T> = std::result::Result<T, Error>;

pub struct Store {
    conn: Mutex<Connection>,
}

const BUILD_COLUMNS: &str = "id, repo_id, number, trigger, ref_name, pr_number, sha, message, actor, trusted, \
     config, state, phase, failure, detail, attempt, not_before, pod, version, rerun_of, cancel_requested, \
     created_at, started_at, finished_at";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        schema::migrate(&conn)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn backup(&self, target: &Path) -> Result<()> {
        let _ = std::fs::remove_file(target);
        self.conn().execute("VACUUM INTO ?1", [target.to_string_lossy()])?;
        Ok(())
    }

    pub fn upsert_repo(&self, repo: &RepoSync) -> Result<()> {
        self.conn().execute(
            "INSERT INTO repos (id, installation_id, owner, name, default_branch, private, archived, last_seen_at, removed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)
             ON CONFLICT(id) DO UPDATE SET installation_id = excluded.installation_id, owner = excluded.owner,
               name = excluded.name, default_branch = excluded.default_branch, private = excluded.private,
               archived = excluded.archived, last_seen_at = excluded.last_seen_at, removed_at = NULL",
            params![repo.id, repo.installation_id, repo.owner, repo.name, repo.default_branch, repo.private, repo.archived, repo.seen_at],
        )?;
        Ok(())
    }

    pub fn mark_removed(&self, keep: &[i64], now: i64) -> Result<()> {
        let conn = self.conn();
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM repos WHERE removed_at IS NULL")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in ids.into_iter().filter(|id| !keep.contains(id)) {
            conn.execute("UPDATE repos SET removed_at = ?2, enrolled = 0 WHERE id = ?1", params![id, now])?;
        }
        Ok(())
    }

    pub fn repos(&self) -> Result<Vec<Repo>> {
        let conn = self.conn();
        let mut statement = conn.prepare(&format!("SELECT {REPO_COLUMNS} FROM repos ORDER BY owner, name"))?;
        Ok(statement.query_map([], repo_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn repo(&self, id: i64) -> Result<Option<Repo>> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {REPO_COLUMNS} FROM repos WHERE id = ?1"), [id], repo_from_row)
            .optional()?)
    }

    pub fn repo_by_name(&self, owner: &str, name: &str) -> Result<Option<Repo>> {
        Ok(self
            .conn()
            .query_row(
                &format!(
                    "SELECT {REPO_COLUMNS} FROM repos WHERE owner = ?1 COLLATE NOCASE AND name = ?2 COLLATE NOCASE"
                ),
                [owner, name],
                repo_from_row,
            )
            .optional()?)
    }

    pub fn set_etag(&self, repo: i64, kind: EtagKind, etag: Option<&str>) -> Result<()> {
        let column = match kind {
            EtagKind::Head => "etag_head",
            EtagKind::Tags => "etag_tags",
            EtagKind::Pulls => "etag_pulls",
        };
        self.conn().execute(&format!("UPDATE repos SET {column} = ?2 WHERE id = ?1"), params![repo, etag])?;
        Ok(())
    }

    pub fn clear_etags(&self, repo: i64) -> Result<()> {
        self.conn()
            .execute("UPDATE repos SET etag_head = NULL, etag_tags = NULL, etag_pulls = NULL WHERE id = ?1", [repo])?;
        Ok(())
    }

    pub fn set_head(&self, repo: i64, sha: &str, enrolled: bool, config_error: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE repos SET head_sha = ?2, enrolled = ?3, config_error = ?4 WHERE id = ?1",
            params![repo, sha, enrolled, config_error],
        )?;
        Ok(())
    }

    pub fn set_paused(&self, repo: i64, paused: bool) -> Result<()> {
        self.conn().execute("UPDATE repos SET paused = ?2 WHERE id = ?1", params![repo, paused])?;
        Ok(())
    }

    pub fn raise_high_water(&self, repo: i64, rc: i64) -> Result<()> {
        self.conn()
            .execute("UPDATE repos SET rc_high_water = MAX(rc_high_water, ?2) WHERE id = ?1", params![repo, rc])?;
        Ok(())
    }

    pub fn record_release(&self, repo: i64, image: &str, tag: &str, now: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO registry_tags (repo_id, image, tag, by_kiln, first_seen) VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT(repo_id, image, tag) DO UPDATE SET by_kiln = 1",
            params![repo, image, tag, now],
        )?;
        Ok(())
    }

    pub fn note_registry_tags(&self, repo: i64, image: &str, tags: &[String], now: i64) -> Result<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let baseline = tx
            .execute("INSERT OR IGNORE INTO registry_scans (repo_id, image) VALUES (?1, ?2)", params![repo, image])?
            == 1;
        let mut unknown = Vec::new();
        for tag in tags {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO registry_tags (repo_id, image, tag, by_kiln, first_seen) VALUES (?1, ?2, ?3, 0, ?4)",
                params![repo, image, tag, now],
            )?;
            if inserted == 1 && !baseline {
                unknown.push(tag.clone());
            }
        }
        tx.commit()?;
        Ok(unknown)
    }

    pub fn set_drift(&self, repo: i64, drift: Option<&str>) -> Result<()> {
        self.conn().execute("UPDATE repos SET drift = ?2 WHERE id = ?1", params![repo, drift])?;
        Ok(())
    }

    pub fn tags_seen(&self, repo: i64) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut statement = conn.prepare("SELECT name FROM seen_tags WHERE repo_id = ?1")?;
        Ok(statement.query_map([repo], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_tags_seen(&self, repo: i64, names: &[String]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for name in names {
            tx.execute("INSERT OR IGNORE INTO seen_tags (repo_id, name) VALUES (?1, ?2)", params![repo, name])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn insert_build(&self, build: &NewBuild) -> Result<Option<i64>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: Option<i64> = tx
            .query_row("SELECT id FROM builds WHERE dedupe_key = ?1", [&build.dedupe_key], |row| row.get(0))
            .optional()?;
        if exists.is_some() {
            return Ok(None);
        }
        let number: i64 = tx.query_row(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM builds WHERE repo_id = ?1",
            [build.repo_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO builds (repo_id, number, trigger, ref_name, pr_number, sha, message, dedupe_key, actor, trusted,
               config, state, failure, detail, rerun_of, created_at, not_before)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, 0)",
            params![
                build.repo_id,
                number,
                build.trigger.as_str(),
                build.ref_name,
                build.pr_number,
                build.sha,
                build.message,
                build.dedupe_key,
                build.actor,
                build.trusted,
                build.config,
                build.state.as_str(),
                build.failure.map(Failure::as_str),
                build.detail,
                build.rerun_of,
                build.created_at,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Some(id))
    }

    pub fn build(&self, id: i64) -> Result<Option<Build>> {
        Ok(self
            .conn()
            .query_row(&format!("SELECT {BUILD_COLUMNS} FROM builds WHERE id = ?1"), [id], build_from_row)
            .optional()?)
    }

    pub fn build_by_number(&self, repo: i64, number: i64) -> Result<Option<Build>> {
        Ok(self
            .conn()
            .query_row(
                &format!("SELECT {BUILD_COLUMNS} FROM builds WHERE repo_id = ?1 AND number = ?2"),
                [repo, number],
                build_from_row,
            )
            .optional()?)
    }

    pub fn builds(&self, filter: &BuildFilter) -> Result<Vec<Build>> {
        let mut sql = format!("SELECT {BUILD_COLUMNS} FROM builds WHERE 1 = 1");
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(repo) = filter.repo {
            values.push(repo.into());
            sql.push_str(&format!(" AND repo_id = ?{}", values.len()));
        }
        if let Some(before) = filter.before {
            values.push(before.into());
            sql.push_str(&format!(" AND id < ?{}", values.len()));
        }
        if !filter.states.is_empty() {
            let list = filter.states.iter().map(|s| format!("'{}'", s.as_str())).collect::<Vec<_>>().join(", ");
            sql.push_str(&format!(" AND state IN ({list})"));
        }
        if let Some(sha) = &filter.sha {
            values.push(sha.clone().into());
            sql.push_str(&format!(" AND sha = ?{}", values.len()));
        }
        values.push(filter.limit.max(1).into());
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", values.len()));
        let conn = self.conn();
        let mut statement = conn.prepare(&sql)?;
        Ok(statement.query_map(rusqlite::params_from_iter(values), build_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    pub fn update_build(&self, id: i64, update: &BuildUpdate) -> Result<()> {
        let mut sets = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        let mut set = |column: &str, value: rusqlite::types::Value| {
            values.push(value);
            sets.push(format!("{column} = ?{}", values.len() + 1));
        };
        if let Some(state) = update.state {
            set("state", state.as_str().to_owned().into());
        }
        if let Some(phase) = &update.phase {
            set("phase", phase.map(|p| p.as_str().to_owned()).into());
        }
        if let Some(failure) = &update.failure {
            set("failure", failure.map(|f| f.as_str().to_owned()).into());
        }
        if let Some(detail) = &update.detail {
            set("detail", detail.clone().into());
        }
        if let Some(pod) = &update.pod {
            set("pod", pod.clone().into());
        }
        if let Some(version) = &update.version {
            set("version", version.clone().into());
        }
        if let Some(config) = &update.config {
            set("config", config.clone().into());
        }
        if let Some(attempt) = update.attempt {
            set("attempt", attempt.into());
        }
        if let Some(not_before) = update.not_before {
            set("not_before", not_before.into());
        }
        if let Some(started_at) = update.started_at {
            set("started_at", started_at.into());
        }
        if let Some(finished_at) = update.finished_at {
            set("finished_at", finished_at.into());
        }
        if let Some(cancel) = update.cancel_requested {
            set("cancel_requested", cancel.into());
        }
        if sets.is_empty() {
            return Ok(());
        }
        let sql = format!("UPDATE builds SET {} WHERE id = ?1", sets.join(", "));
        let mut all: Vec<rusqlite::types::Value> = vec![id.into()];
        all.extend(values);
        self.conn().execute(&sql, rusqlite::params_from_iter(all))?;
        Ok(())
    }

    pub fn queued(&self) -> Result<Vec<Build>> {
        self.builds(&BuildFilter { states: vec![BuildState::Queued], limit: 10_000, ..BuildFilter::default() })
    }

    pub fn active(&self) -> Result<Vec<Build>> {
        self.builds(&BuildFilter {
            states: vec![BuildState::Starting, BuildState::Running, BuildState::Releasing],
            limit: 10_000,
            ..BuildFilter::default()
        })
    }

    pub fn same_line(&self, build: &Build) -> Result<Vec<Build>> {
        let conn = self.conn();
        let mut statement = conn.prepare(&format!(
            "SELECT {BUILD_COLUMNS} FROM builds WHERE repo_id = ?1 AND id < ?2 AND trigger = ?3
               AND COALESCE(pr_number, 0) = COALESCE(?4, 0) AND state IN ('queued', 'starting', 'running')"
        ))?;
        Ok(statement
            .query_map(params![build.repo_id, build.id, build.trigger.as_str(), build.pr_number], build_from_row)?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn replace_tasks(&self, build: i64, attempt: i64, tasks: &[TaskPlan]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM steps WHERE build_id = ?1 AND attempt = ?2", params![build, attempt])?;
        tx.execute("DELETE FROM tasks WHERE build_id = ?1 AND attempt = ?2", params![build, attempt])?;
        for (ordinal, task) in tasks.iter().enumerate() {
            tx.execute(
                "INSERT INTO tasks (build_id, attempt, ordinal, kind, name, container, state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
                params![build, attempt, ordinal as i64, task.kind.as_str(), task.name, task.container],
            )?;
            for (index, step) in task.steps.iter().enumerate() {
                tx.execute(
                    "INSERT INTO steps (build_id, attempt, task, ordinal, name, state) VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                    params![build, attempt, ordinal as i64, index as i64, step],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn tasks(&self, build: i64, attempt: i64) -> Result<Vec<Task>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT ordinal, kind, name, container, state, exit_code, reason, started_at, finished_at, log_final, log_bytes
             FROM tasks WHERE build_id = ?1 AND attempt = ?2 ORDER BY ordinal",
        )?;
        let mut tasks: Vec<Task> = statement
            .query_map(params![build, attempt], |row| {
                Ok(Task {
                    ordinal: row.get(0)?,
                    kind: TaskKind::parse(&row.get::<_, String>(1)?),
                    name: row.get(2)?,
                    container: row.get(3)?,
                    state: TaskState::parse(&row.get::<_, String>(4)?),
                    exit_code: row.get(5)?,
                    reason: row.get(6)?,
                    started_at: row.get(7)?,
                    finished_at: row.get(8)?,
                    log_final: row.get(9)?,
                    log_bytes: row.get(10)?,
                    steps: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut steps = conn.prepare(
            "SELECT task, ordinal, name, state, exit_code, started_at, finished_at, log_offset
             FROM steps WHERE build_id = ?1 AND attempt = ?2 ORDER BY task, ordinal",
        )?;
        let rows = steps.query_map(params![build, attempt], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                Step {
                    ordinal: row.get(1)?,
                    name: row.get(2)?,
                    state: TaskState::parse(&row.get::<_, String>(3)?),
                    exit_code: row.get(4)?,
                    started_at: row.get(5)?,
                    finished_at: row.get(6)?,
                    log_offset: row.get(7)?,
                },
            ))
        })?;
        for row in rows {
            let (task, step) = row?;
            if let Some(task) = tasks.iter_mut().find(|t| t.ordinal == task) {
                task.steps.push(step);
            }
        }
        Ok(tasks)
    }

    pub fn update_task(&self, build: i64, attempt: i64, ordinal: i64, update: &TaskUpdate) -> Result<()> {
        self.conn().execute(
            "UPDATE tasks SET state = COALESCE(?4, state), exit_code = COALESCE(?5, exit_code), reason = COALESCE(?6, reason),
               started_at = COALESCE(started_at, ?7), finished_at = COALESCE(?8, finished_at),
               log_final = COALESCE(?9, log_final), log_bytes = COALESCE(?10, log_bytes)
             WHERE build_id = ?1 AND attempt = ?2 AND ordinal = ?3",
            params![
                build,
                attempt,
                ordinal,
                update.state.map(TaskState::as_str),
                update.exit_code,
                update.reason,
                update.started_at,
                update.finished_at,
                update.log_final,
                update.log_bytes
            ],
        )?;
        Ok(())
    }

    pub fn update_step(&self, build: i64, attempt: i64, task: i64, ordinal: i64, update: &StepUpdate) -> Result<()> {
        self.conn().execute(
            "UPDATE steps SET state = ?5, exit_code = COALESCE(?6, exit_code), started_at = COALESCE(started_at, ?7),
               finished_at = COALESCE(?8, finished_at), log_offset = COALESCE(log_offset, ?9)
             WHERE build_id = ?1 AND attempt = ?2 AND task = ?3 AND ordinal = ?4",
            params![
                build,
                attempt,
                task,
                ordinal,
                update.state.as_str(),
                update.exit_code,
                update.started_at,
                update.finished_at,
                update.log_offset
            ],
        )?;
        Ok(())
    }

    pub fn set_images(&self, build: i64, names: &[String]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM images WHERE build_id = ?1", [build])?;
        for (ordinal, name) in names.iter().enumerate() {
            tx.execute(
                "INSERT INTO images (build_id, ordinal, name) VALUES (?1, ?2, ?3)",
                params![build, ordinal as i64, name],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_candidate(&self, build: i64, name: &str, digest: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE images SET digest = ?3 WHERE build_id = ?1 AND name = ?2", params![build, name, digest])?;
        Ok(())
    }

    pub fn set_released(&self, build: i64, name: &str, tag: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE images SET released = ?3 WHERE build_id = ?1 AND name = ?2", params![build, name, tag])?;
        Ok(())
    }

    pub fn images(&self, build: i64) -> Result<Vec<ImageRecord>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT ordinal, name, digest, released FROM images WHERE build_id = ?1 ORDER BY ordinal")?;
        Ok(statement
            .query_map([build], |row| {
                Ok(ImageRecord { ordinal: row.get(0)?, name: row.get(1)?, digest: row.get(2)?, released: row.get(3)? })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn verified_candidate(&self, repo: i64, sha: &str) -> Result<Option<(i64, Vec<ImageRecord>)>> {
        let found: Option<i64> = self
            .conn()
            .query_row(
                "SELECT b.id FROM builds b WHERE b.repo_id = ?1 AND b.sha = ?2 AND b.state = 'succeeded'
                   AND b.trigger IN ('push', 'manual')
                   AND NOT EXISTS (SELECT 1 FROM images i WHERE i.build_id = b.id AND i.digest IS NULL)
                   AND EXISTS (SELECT 1 FROM images i WHERE i.build_id = b.id)
                 ORDER BY b.id DESC LIMIT 1",
                params![repo, sha],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match found {
            Some(id) => Some((id, self.images(id)?)),
            None => None,
        })
    }

    pub fn check_id(&self, build: i64, name: &str) -> Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row("SELECT check_id FROM checks WHERE build_id = ?1 AND name = ?2", params![build, name], |row| {
                row.get(0)
            })
            .optional()?)
    }

    pub fn set_check_id(&self, build: i64, name: &str, check: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO checks (build_id, name, check_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(build_id, name) DO UPDATE SET check_id = excluded.check_id",
            params![build, name, check],
        )?;
        Ok(())
    }

    pub fn enqueue(&self, build: i64, kind: &str, payload: &serde_json::Value, now: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO outbox (build_id, kind, payload, attempts, next_at) VALUES (?1, ?2, ?3, 0, ?4)",
            params![build, kind, payload.to_string(), now],
        )?;
        Ok(())
    }

    pub fn due(&self, now: i64, limit: i64) -> Result<Vec<OutboxItem>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, build_id, kind, payload, attempts FROM outbox WHERE done_at IS NULL AND next_at <= ?1
             ORDER BY id LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![now, limit], |row| {
                let payload: String = row.get(3)?;
                Ok(OutboxItem {
                    id: row.get(0)?,
                    build_id: row.get(1)?,
                    kind: row.get(2)?,
                    payload: serde_json::from_str(&payload).unwrap_or_default(),
                    attempts: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn outbox_done(&self, id: i64, now: i64) -> Result<()> {
        self.conn().execute("UPDATE outbox SET done_at = ?2 WHERE id = ?1", params![id, now])?;
        Ok(())
    }

    pub fn outbox_retry(&self, id: i64, next_at: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE outbox SET attempts = attempts + 1, next_at = ?2, last_error = ?3 WHERE id = ?1",
            params![id, next_at, error],
        )?;
        Ok(())
    }

    pub fn create_session(&self, session: &Session) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (id_hash, user_id, login, avatar_url, created_at, seen_at, ip, agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
            params![
                session.id_hash,
                session.user_id,
                session.login,
                session.avatar_url,
                session.created_at,
                session.ip,
                session.agent
            ],
        )?;
        Ok(())
    }

    pub fn session(&self, id_hash: &str) -> Result<Option<Session>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id_hash, user_id, login, avatar_url, created_at, seen_at, ip, agent FROM sessions WHERE id_hash = ?1",
                [id_hash],
                |row| {
                    Ok(Session {
                        id_hash: row.get(0)?,
                        user_id: row.get(1)?,
                        login: row.get(2)?,
                        avatar_url: row.get(3)?,
                        created_at: row.get(4)?,
                        seen_at: row.get(5)?,
                        ip: row.get(6)?,
                        agent: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn touch_session(&self, id_hash: &str, now: i64) -> Result<()> {
        self.conn().execute("UPDATE sessions SET seen_at = ?2 WHERE id_hash = ?1", params![id_hash, now])?;
        Ok(())
    }

    pub fn delete_session(&self, id_hash: &str) -> Result<()> {
        self.conn().execute("DELETE FROM sessions WHERE id_hash = ?1", [id_hash])?;
        Ok(())
    }

    pub fn delete_user_sessions(&self, user_id: i64) -> Result<usize> {
        Ok(self.conn().execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?)
    }

    pub fn audit(&self, entry: &AuditEntry) -> Result<()> {
        self.conn().execute(
            "INSERT INTO audit (at, actor, action, repo_id, build_id, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![entry.at, entry.actor, entry.action, entry.repo_id, entry.build_id, entry.detail],
        )?;
        Ok(())
    }

    pub fn audit_log(&self, limit: i64) -> Result<Vec<AuditEntry>> {
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT at, actor, action, repo_id, build_id, detail FROM audit ORDER BY id DESC LIMIT ?1")?;
        Ok(statement
            .query_map([limit], |row| {
                Ok(AuditEntry {
                    at: row.get(0)?,
                    actor: row.get(1)?,
                    action: row.get(2)?,
                    repo_id: row.get(3)?,
                    build_id: row.get(4)?,
                    detail: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn prune(&self, now: i64, retention: &Retention) -> Result<Vec<i64>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let cutoff = now - retention.build_days * 86_400;
        let expired: Vec<i64> = {
            let mut statement = tx.prepare(
                "SELECT id FROM builds b WHERE state IN ('succeeded', 'failed', 'cancelled', 'superseded', 'errored', 'skipped')
                   AND (created_at < ?1 OR (SELECT COUNT(*) FROM builds n WHERE n.repo_id = b.repo_id AND n.id > b.id) >= ?2)",
            )?;
            statement
                .query_map(params![cutoff, retention.builds_per_repo], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in &expired {
            for table in ["steps", "tasks", "images", "checks", "outbox"] {
                tx.execute(&format!("DELETE FROM {table} WHERE build_id = ?1"), [id])?;
            }
            tx.execute("UPDATE builds SET rerun_of = NULL WHERE rerun_of = ?1", [id])?;
            tx.execute("DELETE FROM builds WHERE id = ?1", [id])?;
        }
        tx.execute("DELETE FROM sessions WHERE seen_at < ?1", [now - retention.session_idle_secs])?;
        tx.execute("DELETE FROM sessions WHERE created_at < ?1", [now - retention.session_max_secs])?;
        tx.execute("DELETE FROM outbox WHERE done_at IS NOT NULL AND done_at < ?1", [now - 7 * 86_400])?;
        tx.execute("DELETE FROM audit WHERE at < ?1", [now - 365 * 86_400])?;
        tx.commit()?;
        Ok(expired)
    }
}

const REPO_COLUMNS: &str = "id, installation_id, owner, name, default_branch, private, archived, enrolled, paused, \
     etag_head, etag_tags, etag_pulls, head_sha, rc_high_water, config_error, last_seen_at, removed_at, drift";

fn repo_from_row(row: &Row<'_>) -> rusqlite::Result<Repo> {
    Ok(Repo {
        id: row.get(0)?,
        installation_id: row.get(1)?,
        owner: row.get(2)?,
        name: row.get(3)?,
        default_branch: row.get(4)?,
        private: row.get(5)?,
        archived: row.get(6)?,
        enrolled: row.get(7)?,
        paused: row.get(8)?,
        etag_head: row.get(9)?,
        etag_tags: row.get(10)?,
        etag_pulls: row.get(11)?,
        head_sha: row.get(12)?,
        rc_high_water: row.get(13)?,
        config_error: row.get(14)?,
        last_seen_at: row.get(15)?,
        removed_at: row.get(16)?,
        drift: row.get(17)?,
    })
}

fn build_from_row(row: &Row<'_>) -> rusqlite::Result<Build> {
    Ok(Build {
        id: row.get(0)?,
        repo_id: row.get(1)?,
        number: row.get(2)?,
        trigger: Trigger::parse(&row.get::<_, String>(3)?),
        ref_name: row.get(4)?,
        pr_number: row.get(5)?,
        sha: row.get(6)?,
        message: row.get(7)?,
        actor: row.get(8)?,
        trusted: row.get(9)?,
        config: row.get(10)?,
        state: BuildState::parse(&row.get::<_, String>(11)?),
        phase: row.get::<_, Option<String>>(12)?.map(|p| Phase::parse(&p)),
        failure: row.get::<_, Option<String>>(13)?.map(|f| Failure::parse(&f)),
        detail: row.get(14)?,
        attempt: row.get(15)?,
        not_before: row.get(16)?,
        pod: row.get(17)?,
        version: row.get(18)?,
        rerun_of: row.get(19)?,
        cancel_requested: row.get(20)?,
        created_at: row.get(21)?,
        started_at: row.get(22)?,
        finished_at: row.get(23)?,
    })
}
