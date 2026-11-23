// SPDX-License-Identifier: GPL-3.0-only
//! The log files (architecture §11): one per role and day, `viewer.2026-11-23.log`, at info
//! level, or debug while "Detailed logs" is on in Settings. Files not written for a week are
//! deleted.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing_subscriber::filter::LevelFilter;

/// How long a log file is kept after it was last written.
pub(crate) const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

/// Turns this process's detailed logging on or off: set once the files are open.
static DETAILED: OnceLock<Box<dyn Fn(bool) + Send + Sync>> = OnceLock::new();

/// The level of the files: debug with "Detailed logs", else info.
pub(crate) fn file_level(detailed: bool) -> LevelFilter {
    if detailed {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    }
}

/// Keeps how the level is changed later, by [`set_detailed`].
pub(crate) fn on_detailed(change: Box<dyn Fn(bool) + Send + Sync>) {
    let _ = DETAILED.set(change);
}

/// "Detailed logs" was saved: this process's files follow at once.
pub(crate) fn set_detailed(on: bool) {
    if let Some(change) = DETAILED.get() {
        change(on);
    }
}

/// Days since 1970-01-01, in UTC as the lines' times are.
fn today() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400)
}

/// A day as `YYYY-MM-DD` (the civil calendar from days since 1970, H. Hinnant's way).
fn date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's file of `role` in `dir`.
pub(crate) fn today_file(dir: &Path, role: &str) -> PathBuf {
    file_of(dir, role, today())
}

fn file_of(dir: &Path, role: &str, day: u64) -> PathBuf {
    dir.join(format!("{role}.{}.log", date(day)))
}

/// Deletes the log files in `dir` not written for longer than `keep`, the files of every
/// role and those of versions before 0.16.13 (`viewer.log`) alike.
pub(crate) fn prune(dir: &Path, keep: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok())
            .is_some_and(|age| age > keep);
        if old && path.extension().is_some_and(|x| x == "log") {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The file a role writes to, the next day's from midnight (UTC). Several processes of one
/// role (session windows) append to the same file.
pub(crate) struct DailyLog {
    dir: PathBuf,
    role: String,
    open: Mutex<(u64, File)>,
}

impl DailyLog {
    /// Opens today's file, first deleting what is past keeping.
    pub(crate) fn open(dir: &Path, role: &str) -> std::io::Result<DailyLog> {
        std::fs::create_dir_all(dir)?;
        prune(dir, KEEP);
        let day = today();
        let file = append(&file_of(dir, role, day))?;
        Ok(DailyLog {
            dir: dir.to_path_buf(),
            role: role.to_string(),
            open: Mutex::new((day, file)),
        })
    }
}

fn append(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

impl Write for &DailyLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let day = today();
        if open.0 != day {
            prune(&self.dir, KEEP);
            *open = (day, append(&file_of(&self.dir, &self.role, day))?);
        }
        open.1.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_named_by_its_date() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(19_723), "2024-01-01");
        assert_eq!(date(19_782), "2024-02-29");
        assert_eq!(date(19_783), "2024-03-01");
        assert_eq!(date(20_415), "2025-11-23");
    }

    #[test]
    fn files_not_written_for_a_week_are_deleted() {
        let dir = std::env::temp_dir().join(format!("illum-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let week_ago = SystemTime::now() - KEEP - Duration::from_secs(60);
        for (name, at) in [
            ("viewer.log", week_ago),
            ("service.2020-01-01.log", week_ago),
            ("app.2026-11-23.log", SystemTime::now()),
            ("notes.txt", week_ago),
        ] {
            let file = File::create(dir.join(name)).unwrap();
            file.set_modified(at).unwrap();
        }
        let log = DailyLog::open(&dir, "viewer").unwrap();
        (&log).write_all(b"one line\n").unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let todays = today_file(&dir, "viewer");
        let todays = todays.file_name().unwrap().to_string_lossy().into_owned();
        let mut expected = vec!["app.2026-11-23.log".to_string(), "notes.txt".into(), todays];
        expected.sort();
        assert_eq!(left, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
