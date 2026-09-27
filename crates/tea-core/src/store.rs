//! The lap library on disk: SQLite index + one `.tlap` file per lap.

use crate::frame::Channel;
use crate::recorder::CompletedLap;
use crate::tlap::{self, ChannelEntry, TlapFile, TlapHeader};
use anyhow::Context;
use rusqlite::{params, Connection, Transaction, TransactionBehavior};
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    sim             TEXT NOT NULL,
    track_id        TEXT NOT NULL,
    track_name      TEXT NOT NULL,
    track_config    TEXT NOT NULL,
    track_length_m  REAL NOT NULL,
    car_id          TEXT NOT NULL,
    car_name        TEXT NOT NULL,
    session_type    TEXT NOT NULL,
    started_at_ms   INTEGER NOT NULL,
    conditions_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS laps (
    id                TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL REFERENCES sessions(id),
    lap_number        INTEGER NOT NULL,
    lap_time_ms       INTEGER NOT NULL,
    valid             INTEGER NOT NULL,
    invalid_reason    TEXT,
    fuel_start_l      REAL,
    fuel_used_l       REAL,
    sector_times_json TEXT NOT NULL,
    file_path         TEXT NOT NULL,
    file_missing      INTEGER NOT NULL DEFAULT 0,
    created_at_ms     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS laps_by_session ON laps(session_id);
CREATE INDEX IF NOT EXISTS laps_by_created ON laps(created_at_ms);
CREATE TABLE IF NOT EXISTS track_cache (
    sim            TEXT NOT NULL,
    track_id       TEXT NOT NULL,
    track_config   TEXT NOT NULL,
    outline_lap_id TEXT,
    outline_blob   BLOB,
    corners_json   TEXT,
    updated_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (sim, track_id, track_config)
);
";

#[derive(Clone, Debug, Serialize)]
pub struct LapSummary {
    pub lap_id: String,
    pub session_id: String,
    pub sim: String,
    pub track_name: String,
    pub track_config: String,
    pub car_name: String,
    pub session_type: String,
    pub lap_number: i32,
    pub lap_time_ms: i64,
    pub valid: bool,
    pub invalid_reason: Option<String>,
    pub fuel_used_l: Option<f32>,
    pub created_at_ms: i64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub reindexed: u32,
    pub missing: u32,
    pub temp_removed: u32,
    pub unreadable: u32,
}

pub struct Store {
    root: PathBuf,
    conn: Connection,
}

impl Store {
    pub fn open(root: &Path) -> anyhow::Result<Store> {
        fs::create_dir_all(root.join("laps"))
            .with_context(|| format!("creating {}", root.display()))?;
        let conn = Connection::open(root.join("index.db"))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        migrate(&conn)?;
        Ok(Store { root: root.to_path_buf(), conn })
    }

    /// Write the lap file crash-safely (temp → fsync → rename), then index it.
    pub fn save_lap(&mut self, lap: &CompletedLap, created_at_ms: i64) -> anyhow::Result<()> {
        let (mut header, data) = lap_to_tlap(lap, created_at_ms);
        let bytes = tlap::encode(&mut header, &data)?;
        let rel = format!("laps/{}.tlap", lap.lap_id);
        let final_path = self.root.join(&rel);
        let tmp_path = self.root.join(format!("laps/{}.tlap.tmp", lap.lap_id));
        {
            let mut f = File::create(&tmp_path)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp_path, &final_path)?;
        self.index_lap(&header, &rel)
    }

    pub fn read_lap_file(&self, lap_id: &str) -> anyhow::Result<TlapFile> {
        let rel: String = self
            .conn
            .query_row("SELECT file_path FROM laps WHERE id = ?1", [lap_id], |r| r.get(0))
            .with_context(|| format!("lap {lap_id} not found"))?;
        let bytes = fs::read(self.root.join(rel))?;
        Ok(tlap::decode(&bytes)?)
    }

    pub fn recent_laps(&self, limit: u32) -> anyhow::Result<Vec<LapSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.id, l.session_id, s.sim, s.track_name, s.track_config, s.car_name,
                    s.session_type, l.lap_number, l.lap_time_ms, l.valid, l.invalid_reason,
                    l.fuel_used_l, l.created_at_ms
             FROM laps l JOIN sessions s ON s.id = l.session_id
             WHERE l.file_missing = 0
             ORDER BY l.created_at_ms DESC, l.lap_number DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(LapSummary {
                lap_id: r.get(0)?,
                session_id: r.get(1)?,
                sim: r.get(2)?,
                track_name: r.get(3)?,
                track_config: r.get(4)?,
                car_name: r.get(5)?,
                session_type: r.get(6)?,
                lap_number: r.get(7)?,
                lap_time_ms: r.get(8)?,
                valid: r.get(9)?,
                invalid_reason: r.get(10)?,
                fuel_used_l: r.get::<_, Option<f64>>(11)?.map(|v| v as f32),
                created_at_ms: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Bring the index in line with the files on disk after a crash or manual file changes.
    pub fn reconcile(&mut self) -> anyhow::Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let mut on_disk = HashSet::new();
        for entry in fs::read_dir(self.root.join("laps"))? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
            if name.ends_with(".tlap.tmp") {
                // A leftover we can't delete (e.g. locked by antivirus) is harmless; it
                // must not stop the app from starting.
                match fs::remove_file(&path) {
                    Ok(()) => report.temp_removed += 1,
                    Err(e) => log::warn!("{name}: couldn't remove leftover temp file: {e}"),
                }
                continue;
            }
            let Some(id) = name.strip_suffix(".tlap") else { continue };
            on_disk.insert(id.to_string());
            let known: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM laps WHERE id = ?1)",
                [id],
                |r| r.get(0),
            )?;
            if known {
                self.conn.execute("UPDATE laps SET file_missing = 0 WHERE id = ?1", [id])?;
                continue;
            }
            let decoded = fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|b| Ok(tlap::decode(&b)?));
            match decoded {
                Ok(file) if file.header.lap_id == id => {
                    self.index_lap(&file.header, &format!("laps/{name}"))?;
                    report.reindexed += 1;
                }
                Ok(_) => {
                    log::warn!("{name}: lap id in header doesn't match file name; skipped");
                    report.unreadable += 1;
                }
                Err(e) => {
                    log::warn!("{name}: {e:#}; skipped");
                    report.unreadable += 1;
                }
            }
        }
        let ids: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT id FROM laps WHERE file_missing = 0")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in ids.into_iter().filter(|id| !on_disk.contains(id)) {
            self.conn.execute("UPDATE laps SET file_missing = 1 WHERE id = ?1", [&id])?;
            report.missing += 1;
        }
        Ok(report)
    }

    fn index_lap(&mut self, h: &TlapHeader, rel_path: &str) -> anyhow::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO sessions (id, sim, track_id, track_name, track_config,
                 track_length_m, car_id, car_name, session_type, started_at_ms, conditions_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                h.session_id,
                h.session.sim.as_str(),
                h.session.track_id,
                h.session.track_name,
                h.session.track_config,
                h.session.track_length_m as f64,
                h.session.car_id,
                h.session.car_name,
                h.session.session_type,
                h.session_started_at_ms,
                serde_json::to_string(&h.session.conditions)?,
            ],
        )?;
        tx.execute(
            "INSERT OR REPLACE INTO laps (id, session_id, lap_number, lap_time_ms, valid,
                 invalid_reason, fuel_start_l, fuel_used_l, sector_times_json, file_path,
                 file_missing, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11)",
            params![
                h.lap_id,
                h.session_id,
                h.lap_number,
                h.lap_time_ms,
                h.valid,
                h.invalid_reason,
                h.fuel_start_l.map(|v| v as f64),
                h.fuel_used_l.map(|v| v as f64),
                serde_json::to_string(&h.sector_times_ms)?,
                rel_path,
                h.created_at_ms,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")?;
    let version: Option<i64> =
        tx.query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))?;
    match version {
        None => {
            tx.execute_batch(SCHEMA_V1)?;
            tx.execute("INSERT INTO schema_version (version) VALUES (?1)", [SCHEMA_VERSION])?;
        }
        Some(v) if v == SCHEMA_VERSION => {}
        Some(v) if v > SCHEMA_VERSION => anyhow::bail!(
            "index.db was created by a newer version of Tea Telemetry (schema {v}); please update the app"
        ),
        Some(v) => anyhow::bail!("unsupported index.db schema version {v}"),
    }
    tx.commit()?;
    Ok(())
}

fn lap_to_tlap(lap: &CompletedLap, created_at_ms: i64) -> (TlapHeader, Vec<Vec<f32>>) {
    let mut channels = vec![ChannelEntry { name: "t_s".into(), unit: "s".into(), offset: 0 }];
    let mut data: Vec<Vec<f32>> = vec![lap
        .samples
        .iter()
        .map(|f| (f.session_time_s - lap.lap_start_s) as f32)
        .collect()];
    for &ch in Channel::ALL {
        if lap.samples.iter().any(|f| f.get(ch).is_finite()) {
            channels.push(ChannelEntry { name: ch.name().into(), unit: ch.unit().into(), offset: 0 });
            data.push(lap.samples.iter().map(|f| f.get(ch)).collect());
        }
    }
    let header = TlapHeader {
        format_version: tlap::FORMAT_VERSION,
        lap_id: lap.lap_id.clone(),
        session_id: lap.session_id.clone(),
        session_started_at_ms: lap.session_started_at_ms,
        session: lap.session.clone(),
        lap_number: lap.lap_number,
        lap_time_ms: lap.lap_time_ms,
        valid: lap.valid(),
        invalid_reason: lap.invalid_reason.map(|r| r.as_str().to_string()),
        fuel_start_l: lap.fuel_start_l,
        fuel_used_l: lap.fuel_used_l,
        sector_times_ms: lap.sector_times_ms.clone(),
        created_at_ms,
        sample_count: 0,
        channels,
    };
    (header, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::completed_lap;
    use tempfile::tempdir;

    #[test]
    fn save_lap_writes_file_and_index_row() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 777).unwrap();

        let laps = store.recent_laps(10).unwrap();
        assert_eq!(laps.len(), 1);
        let s = &laps[0];
        assert_eq!(s.lap_id, lap.lap_id);
        assert_eq!(s.sim, "iracing");
        assert_eq!(s.track_name, "Circuit de Spa-Francorchamps");
        assert_eq!(s.car_name, "Porsche 963 GTP");
        assert_eq!(s.lap_number, 2);
        assert_eq!(s.lap_time_ms, 100_000);
        assert!(s.valid);
        assert_eq!(s.created_at_ms, 777);
        assert!(dir.path().join(format!("laps/{}.tlap", lap.lap_id)).exists());
    }

    #[test]
    fn lap_file_has_time_first_and_only_present_channels() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 1).unwrap();

        let file = store.read_lap_file(&lap.lap_id).unwrap();
        let names: Vec<&str> = file.header.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names[0], "t_s");
        assert!(names.contains(&"lap_dist_pct"));
        assert!(names.contains(&"speed"));
        assert!(!names.contains(&"pos_x"), "absent channels must not be stored");
        assert_eq!(file.header.sample_count as usize, lap.samples.len());
        let t = &file.data[0];
        assert!(t[0] < 0.0 && t[1] >= 0.0, "first boundary sample is before the line");
        assert_eq!(file.header.sector_times_ms, vec![30_000, 40_000, 30_000]);
    }

    #[test]
    fn reconcile_reindexes_orphan_files() {
        let dir = tempdir().unwrap();
        let lap = completed_lap();
        {
            let mut store = Store::open(dir.path()).unwrap();
            store.save_lap(&lap, 1).unwrap();
        }
        // Simulate a crash between writing the file and indexing it: lose the index.
        for f in ["index.db", "index.db-wal", "index.db-shm"] {
            let _ = std::fs::remove_file(dir.path().join(f));
        }
        let mut store = Store::open(dir.path()).unwrap();
        assert!(store.recent_laps(10).unwrap().is_empty());
        let report = store.reconcile().unwrap();
        assert_eq!(report.reindexed, 1);
        let laps = store.recent_laps(10).unwrap();
        assert_eq!(laps.len(), 1);
        assert_eq!(laps[0].lap_id, lap.lap_id);
        assert_eq!(laps[0].lap_time_ms, 100_000);
    }

    #[test]
    fn reconcile_cleans_temp_and_flags_missing() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 1).unwrap();
        std::fs::write(dir.path().join("laps/half-written.tlap.tmp"), b"partial").unwrap();
        std::fs::remove_file(dir.path().join(format!("laps/{}.tlap", lap.lap_id))).unwrap();

        let report = store.reconcile().unwrap();
        assert_eq!(report.temp_removed, 1);
        assert_eq!(report.missing, 1);
        assert!(!dir.path().join("laps/half-written.tlap.tmp").exists());
        assert!(store.recent_laps(10).unwrap().is_empty(), "missing laps are hidden");
    }

    #[test]
    fn reconcile_survives_a_temp_file_it_cannot_remove() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let lap = completed_lap();
        store.save_lap(&lap, 1).unwrap();
        // remove_file fails on a directory, standing in for a locked file.
        std::fs::create_dir(dir.path().join("laps/stuck.tlap.tmp")).unwrap();
        std::fs::write(dir.path().join("laps/half-written.tlap.tmp"), b"partial").unwrap();

        let report = store.reconcile().expect("one bad temp file must not fail startup");
        assert_eq!(report.temp_removed, 1);
        assert!(!dir.path().join("laps/half-written.tlap.tmp").exists());
        assert_eq!(store.recent_laps(10).unwrap().len(), 1);
    }

    #[test]
    fn reconcile_skips_unreadable_files_without_deleting_them() {
        let dir = tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let junk = dir.path().join("laps/not-really.tlap");
        std::fs::write(&junk, b"garbage").unwrap();
        let report = store.reconcile().unwrap();
        assert_eq!(report.unreadable, 1);
        assert!(junk.exists());
    }

    #[test]
    fn refuses_database_from_newer_version() {
        let dir = tempdir().unwrap();
        Store::open(dir.path()).unwrap();
        let conn = Connection::open(dir.path().join("index.db")).unwrap();
        conn.execute("INSERT INTO schema_version (version) VALUES (99)", []).unwrap();
        drop(conn);
        let err = Store::open(dir.path()).err().expect("should refuse").to_string();
        assert!(err.contains("newer version"), "{err}");
    }

    #[test]
    fn open_is_idempotent() {
        let dir = tempdir().unwrap();
        Store::open(dir.path()).unwrap();
        Store::open(dir.path()).unwrap();
    }
}
