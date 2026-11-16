//! `hgm await`: the Goal Manager waits for its next wake as a background
//! command instead of an idle input line. While an await runs for the tab, the
//! watch hands it the wake it would have typed, so a draft in the manager's
//! input no longer holds a wake back, and a Claude Code manager with a native
//! goal does not spin: Claude Code holds the goal's check while a background
//! command runs and starts a new turn when it ends.
//!
//! The hand-over is two files under the tab's key: the await holds an
//! exclusive lock on `-await.lock` while it runs, and the watch writes the
//! wake to `-await.json`. Whoever takes the wake renames that file away first,
//! so a wake is printed by one await or taken back by the watch, never both.
use crate::{
    binding::{path, session},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
    time::{Duration, Instant},
};

/// How long `hgm await` waits by default: news arrives as a wake well within
/// it (patrols run every few minutes while the worker works).
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 1800;
/// How often the await looks for a handed wake and at the watch record.
const POLL: Duration = Duration::from_millis(500);

fn lock_path(store: &Store, key: &str) -> Result<PathBuf> {
    path(store, key, "-await.lock")
}
fn handed_path(store: &Store, key: &str) -> Result<PathBuf> {
    path(store, key, "-await.json")
}
fn open_lock(store: &Store, key: &str) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path(store, key)?)?)
}

/// How long the lock must stay held before [`running`] believes an await
/// holds it. A process spawned by another thread holds a copy of a lock this
/// process just released until it executes its program, so a released lock
/// can look held for a moment: the watch's own probe of a free lock can leave
/// such a copy, and the next probe would then see an await that does not run
/// (a unit test failed on it in 5 of 300 runs beside tests that spawn
/// processes, 2026-11-16). An await holds its lock for its whole life.
const HELD_FOR: Duration = Duration::from_millis(100);
/// How often [`running`] looks again while the lock is held.
const RECHECK: Duration = Duration::from_millis(20);

/// Whether an `hgm await` runs for the tab under `key`: its lock stays held
/// for [`HELD_FOR`]. A free lock answers at once; a held one takes that long.
pub(crate) fn running(store: &Store, key: &str) -> Result<bool> {
    let deadline = Instant::now() + HELD_FOR;
    loop {
        if open_lock(store, key)?.try_lock().is_ok() {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Ok(true);
        }
        std::thread::sleep(RECHECK);
    }
}

/// Takes the handed wake, if one waits, for `taker` (`await` or `watch`).
/// The rename decides who has it when both try at once.
fn claim(store: &Store, key: &str, taker: &str) -> Result<Option<Value>> {
    let handed = handed_path(store, key)?;
    let taken = handed.with_extension(format!("{taker}-{}.json", std::process::id()));
    match std::fs::rename(&handed, &taken) {
        Ok(()) => {
            let bytes = std::fs::read(&taken).context("read the handed wake")?;
            std::fs::remove_file(&taken).ok();
            Ok(Some(serde_json::from_slice(&bytes)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("take the handed wake"),
    }
}

/// What became of a wake offered to the await.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    /// No await runs: type the wake as before.
    NoAwait,
    /// An earlier handed wake has not been taken yet: keep this one pending.
    Busy,
    /// The await has it.
    Taken,
}

/// Hands `notice` about `goal` to the running await of the tab under `key`.
/// When the await ended while the wake was written, the watch takes it back
/// unless the await already took it.
pub(crate) fn hand_over(store: &Store, key: &str, goal: &str, notice: &str) -> Result<Offer> {
    if !running(store, key)? {
        return Ok(Offer::NoAwait);
    }
    let handed = handed_path(store, key)?;
    if handed.exists() {
        return Ok(Offer::Busy);
    }
    store.write_private(
        &handed,
        &json!({"goal": goal, "at": Utc::now().timestamp(), "notice": notice}),
    )?;
    if running(store, key)? || claim(store, key, "watch")?.is_none() {
        return Ok(Offer::Taken);
    }
    Ok(Offer::NoAwait)
}

/// Why `hgm await` returned.
fn ended(reason: &str, text: String) -> Value {
    json!({"ended": reason, "text": text})
}

/// `hgm await`: waits until the tab's watch hands over a wake, the watch ends
/// or `timeout` passes, and returns what to print. A wake handed to an earlier
/// await that ended without printing it comes first.
pub(crate) async fn wait(store: &Store, timeout: Duration) -> Result<Value> {
    let key = session(store)?.key();
    let lock = open_lock(store, &key)?;
    // The watch briefly takes the lock to see whether an await runs.
    let mut locked = lock.try_lock().is_ok();
    for _ in 0..5 {
        if locked {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        locked = lock.try_lock().is_ok();
    }
    ensure!(
        locked,
        "another hgm await already waits for this Goal Manager; let it run (its output arrives when it ends) or stop it before starting another"
    );
    let watch_path = crate::watch::watch_path(store, &key)?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(wake) = claim(store, &key, "await")? {
            return Ok(wake);
        }
        let now = Utc::now().timestamp();
        let watch = watch_path
            .exists()
            .then(|| crate::watch::load_watch(store, &key))
            .transpose()?;
        let watching = match &watch {
            Some(watch) => watch.active && crate::watch::watch_running(store, watch, now)?,
            None => false,
        };
        if !watching || Instant::now() >= deadline {
            // Released first, so a wake the watch writes from now on is
            // either taken back by it or taken here.
            drop(lock);
            if let Some(wake) = claim(store, &key, "await")? {
                return Ok(wake);
            }
            return Ok(match watch {
                _ if watching => ended(
                    "timeout",
                    format!(
                        "No wake for this Goal Manager within {} s; the watch still runs. Run hgm await again in the background to keep waiting, or check hgm status.",
                        timeout.as_secs()
                    ),
                ),
                Some(watch) if !watch.active => ended(
                    "watch_ended",
                    format!(
                        "The watch of this tab has ended ({}); no wake will follow. Check hgm status and hgm inbox, and tell the user what needs them.",
                        watch.status
                    ),
                ),
                _ => ended(
                    "not_watched",
                    "No watch runs for this tab, so no wake would come. A linked hgm send or hgm supervise starts one; otherwise check hgm status and stop here.".into(),
                ),
            });
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The text `hgm await` prints for `result`: the wake as the watch would
/// have typed it, or why no wake came.
pub(crate) fn text(result: &Value) -> String {
    result["notice"]
        .as_str()
        .or_else(|| result["text"].as_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("work");
        std::fs::create_dir(&workspace).unwrap();
        let store = Store::new(&workspace, &root.path().join("state")).unwrap();
        (root, store)
    }

    #[test]
    fn a_lock_held_only_for_a_moment_is_not_a_running_await() {
        // As a copy left in a process spawned by another thread: the lock is
        // released 30 ms later, so no await runs.
        let (_root, store) = store();
        let key = "t-original";
        let copy = open_lock(&store, key).unwrap();
        copy.try_lock().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(copy);
        });
        let start = Instant::now();
        assert!(!running(&store, key).unwrap());
        assert!(start.elapsed() >= Duration::from_millis(20));
        release.join().unwrap();
        // An await holds its lock throughout: it runs.
        let held = open_lock(&store, key).unwrap();
        held.try_lock().unwrap();
        let start = Instant::now();
        assert!(running(&store, key).unwrap());
        assert!(start.elapsed() >= HELD_FOR);
    }

    #[test]
    fn a_wake_goes_to_a_running_await_and_is_taken_once() {
        let (_root, store) = store();
        let key = "t-original";
        assert_eq!(
            hand_over(&store, key, "g-1", "first").unwrap(),
            Offer::NoAwait,
            "without an await the wake is typed as before"
        );
        assert!(!handed_path(&store, key).unwrap().exists());
        let lock = open_lock(&store, key).unwrap();
        lock.try_lock().unwrap();
        assert!(running(&store, key).unwrap());
        assert_eq!(
            hand_over(&store, key, "g-1", "first").unwrap(),
            Offer::Taken
        );
        assert_eq!(
            hand_over(&store, key, "g-1", "second").unwrap(),
            Offer::Busy,
            "one handed wake at a time"
        );
        let taken = claim(&store, key, "await").unwrap().unwrap();
        assert_eq!(text(&taken), "first");
        assert!(claim(&store, key, "watch").unwrap().is_none(), "taken once");
        // An await that ended between the write and the check: the watch
        // takes its wake back and types it; the await finds nothing.
        assert_eq!(
            hand_over(&store, key, "g-1", "third").unwrap(),
            Offer::Taken
        );
        drop(lock);
        assert!(!running(&store, key).unwrap());
        assert_eq!(
            text(&claim(&store, key, "watch").unwrap().unwrap()),
            "third"
        );
        assert!(claim(&store, key, "await").unwrap().is_none());
    }
}
