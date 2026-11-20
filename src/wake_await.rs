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

pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 1800;
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

// A forked child can briefly retain the probe's lock until exec.
const HELD_FOR: Duration = Duration::from_millis(100);
const RECHECK: Duration = Duration::from_millis(20);

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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Offer {
    NoAwait,
    Busy,
    Taken,
}

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

fn ended(reason: &str, text: String) -> Value {
    json!({"ended": reason, "text": text})
}

pub(crate) async fn wait(store: &Store, timeout: Duration) -> Result<Value> {
    let key = session(store)?.key();
    let lock = open_lock(store, &key)?;
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
