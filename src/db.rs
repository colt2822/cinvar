use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::paths;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    model TEXT,
    title TEXT NOT NULL,
    transcript TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    project_id TEXT,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    detail TEXT
);
CREATE TABLE IF NOT EXISTS usage (
    id TEXT PRIMARY KEY,
    session_id TEXT,
    model TEXT,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    usd REAL NOT NULL DEFAULT 0,
    at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS analysis_cache (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    created_at TEXT NOT NULL
);
";

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
}

/// (id, kind, status, started_at, detail)
pub type JobRow = (String, String, String, String, Option<String>);

impl Db {
    pub fn default_path() -> PathBuf {
        paths::data_dir().join("cinvar.db")
    }

    pub fn open(path: &Path) -> Result<Db> {
        if let Some(parent) = path.parent() {
            paths::ensure_dir(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Db> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    pub fn upsert_project(&self, id: &str, path: &str, name: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO projects (id, path, name, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET path=?2, name=?3, updated_at=?4",
            params![id, path, name, crate::util::clock::now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn projects(&self) -> Result<Vec<(String, String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, path, name, updated_at FROM projects ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn forget_project(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn record_job(
        &self,
        id: &str,
        kind: &str,
        project_id: Option<&str>,
        status: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO jobs (id, kind, project_id, status, started_at, finished_at, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)
             ON CONFLICT(id) DO UPDATE SET status=?4, detail=?6,
               finished_at=CASE WHEN ?4 IN ('succeeded','failed','cancelled') THEN ?5 ELSE finished_at END",
            params![id, kind, project_id, status, crate::util::clock::now_rfc3339(), detail],
        )?;
        Ok(())
    }

    pub fn record_job_finished(
        &self,
        id: &str,
        kind: &str,
        project_id: Option<&str>,
        status: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        self.record_job(id, kind, project_id, status, detail)
    }

    pub fn recent_jobs(&self, limit: usize) -> Result<Vec<JobRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, status, started_at, detail FROM jobs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn save_session(&self, row: &SessionRow, transcript: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, project_id, model, title, transcript, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(id) DO UPDATE SET title=?4, transcript=?5, updated_at=?6, model=?3",
            params![row.id, row.project_id, row.model, row.title, transcript, row.created_at],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self, project_id: Option<&str>) -> Result<Vec<SessionRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, model, title, created_at, updated_at FROM sessions
             WHERE (?1 IS NULL OR project_id = ?1) ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![project_id], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                project_id: r.get(1)?,
                model: r.get(2)?,
                title: r.get(3)?,
                created_at: r.get(4)?,
                updated_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn load_transcript(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT transcript FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            Ok(row.get::<_, Option<String>>(0)?)
        } else {
            Ok(None)
        }
    }

    pub fn record_usage(
        &self,
        session_id: &str,
        model: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
        usd: f64,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let id = crate::util::ids::new_id("usage");
        conn.execute(
            "INSERT INTO usage (id, session_id, model, prompt_tokens, completion_tokens, usd, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                session_id,
                model,
                prompt_tokens as i64,
                completion_tokens as i64,
                usd,
                crate::util::clock::now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn usage_totals(&self, session_id: Option<&str>) -> Result<(u64, u64, f64)> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0), COALESCE(SUM(usd),0)
             FROM usage WHERE (?1 IS NULL OR session_id = ?1)",
        )?;
        let mut rows = stmt.query(params![session_id])?;
        if let Some(row) = rows.next()? {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, i64>(1)? as u64,
                row.get::<_, f64>(2)?,
            ))
        } else {
            Ok((0, 0, 0.0))
        }
    }

    pub fn cache_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM analysis_cache WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub fn cache_put(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO analysis_cache (key, value, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value=?2, created_at=?3",
            params![key, value, crate::util::clock::now_rfc3339()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_projects_and_usage() {
        let db = Db::open_in_memory().unwrap();
        db.upsert_project("p1", "/tmp/p1", "Project One").unwrap();
        assert_eq!(db.projects().unwrap().len(), 1);
        db.record_usage("s1", "mock", 10, 20, 0.0).unwrap();
        let (p, c, _) = db.usage_totals(Some("s1")).unwrap();
        assert_eq!((p, c), (10, 20));
        db.cache_put("k", "v").unwrap();
        assert_eq!(db.cache_get("k").unwrap().as_deref(), Some("v"));
    }
}
