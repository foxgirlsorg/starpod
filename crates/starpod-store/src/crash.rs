//! Crash reports: files of their own, so they survive log rotation.
//!
//! The log file is replaced on every start (the previous run's log is kept
//! once, then dropped), so a crash can be lost with the log that held it. A
//! crash report is written as a separate file in the crashes folder, named by
//! the UTC time it was written, and only the newest [`KEEP`] reports of each
//! kind are kept.
//!
//! Writing is best effort. A crash report is written from a panic hook or
//! from the engine's exit path, where a failure must not cause another
//! failure, so every error is absorbed and reported as `None`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use time::OffsetDateTime;
use time::macros::format_description;

/// How many reports of one kind are kept. The oldest are deleted first.
pub const KEEP: usize = 20;

/// How many names to try when reports of the same kind share one second.
const SAME_SECOND_ATTEMPTS: u32 = 100;

/// Write `body` to `<dir>/<prefix>-<UTC timestamp>.log`, creating `dir` when
/// it is missing, then delete all but the newest [`KEEP`] files named
/// `<prefix>-*.log` in `dir`.
///
/// Returns the path that was written, or `None` when nothing could be
/// written.
pub fn write_report(dir: &Path, prefix: &str, body: &str) -> Option<PathBuf> {
    fs::create_dir_all(dir).ok()?;
    let stamp = OffsetDateTime::now_utc()
        .format(format_description!(
            "[year][month][day]-[hour][minute][second]"
        ))
        .ok()?;
    // A second report of the same kind in the same second gets a numbered
    // name rather than replacing the first.
    let (path, mut file) = (1..=SAME_SECOND_ATTEMPTS).find_map(|attempt| {
        let name = if attempt == 1 {
            format!("{prefix}-{stamp}.log")
        } else {
            format!("{prefix}-{stamp}-{attempt}.log")
        };
        let path = dir.join(name);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .ok()
            .map(|file| (path, file))
    })?;
    file.write_all(body.as_bytes()).ok()?;
    let _ = file.sync_all();
    prune(dir, prefix);
    Some(path)
}

/// Delete the oldest `<prefix>-*.log` files in `dir`, keeping [`KEEP`].
///
/// The names carry a sortable UTC timestamp, so sorting by name sorts by
/// age.
fn prune(dir: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let start = format!("{prefix}-");
    let mut reports: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&start) && name.ends_with(".log"))
        })
        .collect();
    reports.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    let excess = reports.len().saturating_sub(KEEP);
    for path in reports.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
}
