//! Lightweight SQLite store for desktop-side metadata.
//!
//! Stores a small set of key-value pairs about the desktop app itself and a
//! historical record of every Harness version that has been installed. All
//! data lives under `<app_data_dir>/dsh-desktop.db`.
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, so the handle is wrapped
//! in a plain `std::sync::Mutex`. All operations are fast point queries, so
//! blocking briefly on the mutex is acceptable and keeps the API synchronous.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Clone)]
pub(crate) struct DesktopDb {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HarnessHistoryEntry {
    pub version: String,
    pub install_time: String,
    pub source: String,
    pub is_current: bool,
}

impl DesktopDb {
    /// Opens (or creates) the database at `<data_dir>/dsh-desktop.db` and runs
    /// migrations on first launch.
    pub(crate) fn open(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|error| format!("无法创建应用数据目录：{error}"))?;
        let db_path = data_dir.join("dsh-desktop.db");
        let conn = Connection::open(&db_path)
            .map_err(|error| format!("无法打开数据库 {}：{error}", db_path.display()))?;

        conn.execute_batch("PRAGMA journal_mode = WAL;")
            .map_err(|error| format!("无法设置数据库参数：{error}"))?;

        Self::migrate(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn migrate(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS app_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS harness_history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                version      TEXT    NOT NULL UNIQUE,
                install_time TEXT    NOT NULL,
                source       TEXT    NOT NULL DEFAULT 'bundled',
                is_current   INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_harness_history_version
                ON harness_history (version);
            ",
        )
        .map_err(|error| format!("数据库迁移失败：{error}"))?;
        Ok(())
    }

    // ── Key-value helpers ──

    pub(crate) fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .lock()
            .map_err(|_| "数据库锁中毒".to_owned())?
            .execute(
                "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|error| format!("写入元数据失败：{error}"))?;
        Ok(())
    }

    pub(crate) fn get_meta(&self, key: &str) -> Result<Option<String>, String> {
        self.conn
            .lock()
            .map_err(|_| "数据库锁中毒".to_owned())?
            .query_row(
                "SELECT value FROM app_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("读取元数据失败：{error}"))
    }

    // ── Harness version history ─

    /// Records a Harness version that was installed or activated. Marks it as
    /// current and clears the flag on all previous entries. Repeated records
    /// of the same version refresh the timestamp instead of duplicating rows.
    pub(crate) fn record_harness_version(
        &self,
        version: &str,
        source: &str,
    ) -> Result<(), String> {
        let now = now_iso();
        let conn = self
            .conn
            .lock()
            .map_err(|_| "数据库锁中毒".to_owned())?;
        conn.execute("UPDATE harness_history SET is_current = 0", [])
            .map_err(|error| format!("清除历史标记失败：{error}"))?;
        conn.execute(
            "INSERT INTO harness_history (version, install_time, source, is_current)
             VALUES (?1, ?2, ?3, 1)
             ON CONFLICT(version) DO UPDATE SET
                 install_time = excluded.install_time,
                 source = excluded.source,
                 is_current = 1",
            params![version, now, source],
        )
        .map_err(|error| format!("记录版本历史失败：{error}"))?;
        Ok(())
    }

    /// Returns all installed Harness versions, most recent first.
    pub(crate) fn harness_history(&self) -> Result<Vec<HarnessHistoryEntry>, String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "数据库锁中毒".to_owned())?;
        let mut stmt = conn
            .prepare(
                "SELECT version, install_time, source, is_current
                 FROM harness_history
                 ORDER BY install_time DESC, id DESC",
            )
            .map_err(|error| format!("查询版本历史失败：{error}"))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(HarnessHistoryEntry {
                    version: row.get(0)?,
                    install_time: row.get(1)?,
                    source: row.get(2)?,
                    is_current: row.get::<_, i32>(3)? != 0,
                })
            })
            .map_err(|error| format!("读取版本历史失败：{error}"))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("读取版本历史失败：{error}"))
    }
}

/// Current UTC time as an ISO-8601 string, without pulling in chrono.
pub(crate) fn now_iso() -> String {
    use std::time::SystemTime;
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Days → year/month/day via the civil-from-days algorithm.
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    fn temp_db() -> DesktopDb {
        let dir = temp_dir().join(format!(
            "dsh-db-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        DesktopDb::open(&dir).expect("temp db should open")
    }

    #[test]
    fn set_and_get_meta() {
        let db = temp_db();
        db.set_meta("theme", "dark").unwrap();
        assert_eq!(db.get_meta("theme").unwrap(), Some("dark".into()));
        assert_eq!(db.get_meta("missing").unwrap(), None);
    }

    #[test]
    fn upsert_meta() {
        let db = temp_db();
        db.set_meta("key", "v1").unwrap();
        db.set_meta("key", "v2").unwrap();
        assert_eq!(db.get_meta("key").unwrap(), Some("v2".into()));
    }

    #[test]
    fn records_harness_history() {
        let db = temp_db();
        db.record_harness_version("0.1.0-rc.5", "bundled").unwrap();
        db.record_harness_version("0.1.0-rc.6", "update").unwrap();
        let history = db.harness_history().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].version, "0.1.0-rc.6");
        assert!(history[0].is_current);
        assert!(!history[1].is_current);
    }

    #[test]
    fn re_recording_same_version_does_not_duplicate() {
        let db = temp_db();
        db.record_harness_version("0.1.0-rc.6", "bundled").unwrap();
        db.record_harness_version("0.1.0-rc.6", "bundled").unwrap();
        let history = db.harness_history().unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].is_current);
    }

    #[test]
    fn now_iso_is_utc_shaped() {
        let ts = now_iso();
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }
}
