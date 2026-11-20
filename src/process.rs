use crate::{model::CommandSpec, store::Store};
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Seek, SeekFrom},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

pub(crate) async fn execute(
    store: &Store,
    dir: &std::path::Path,
    id: &str,
    spec: &CommandSpec,
    prompt: &str,
    timeout_secs: u64,
) -> Result<String> {
    let RunOutput {
        status,
        mut stdout,
        mut stderr,
        directory,
    } = run(store, dir, id, spec, prompt, timeout_secs).await?;
    ensure!(
        status.success(),
        "runner exited with {status}; {}\nLogs: {}",
        tail(&mut stderr, 8192)?,
        directory.display()
    );
    ensure!(
        stdout.metadata()?.len() <= 262_144,
        "runner output exceeds 256 KiB; inspect {}",
        directory.display()
    );
    stdout.seek(SeekFrom::Start(0))?;
    let mut output = String::new();
    stdout
        .read_to_string(&mut output)
        .context("runner stdout must be UTF-8")?;
    Ok(output)
}

struct RunOutput {
    status: std::process::ExitStatus,
    stdout: std::fs::File,
    stderr: std::fs::File,
    directory: std::path::PathBuf,
}

pub(crate) async fn predicate(
    store: &Store,
    dir: &std::path::Path,
    id: &str,
    spec: &CommandSpec,
    timeout_secs: u64,
) -> Result<bool> {
    let output = run(store, dir, id, spec, "", timeout_secs).await?;
    output.status.code().map(|code| code == 0).with_context(|| {
        format!(
            "stop check exited with {}; inspect {}",
            output.status,
            output.directory.display()
        )
    })
}

async fn run(
    store: &Store,
    dir: &std::path::Path,
    id: &str,
    spec: &CommandSpec,
    prompt: &str,
    timeout_secs: u64,
) -> Result<RunOutput> {
    let directory = store.run_directory(id)?;
    std::io::Write::write_all(
        &mut open_log(&directory.join("prompt.txt"))?,
        prompt.as_bytes(),
    )?;
    let stdout = open_log(&directory.join("stdout.log"))?;
    let stderr = open_log(&directory.join("stderr.log"))?;
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(dir)
        .env("HGM_PROMPT", prompt)
        .env("HGM_WORKSPACE", &store.workspace)
        .env("HGM_RUN_ID", id)
        .env("HGM_RUN_DIR", &directory)
        .stdin(if spec.prompt_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?))
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("start configured runner {}", spec.program))?;
    let group = ProcessGroup(child.id());
    let outcome = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .context("send runner prompt")?;
            drop(stdin);
        }
        child.wait().await.context("wait for runner")
    })
    .await;
    let status = match outcome {
        Ok(result) => result?,
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!(
                "runner timed out after {timeout_secs}s; logs: {}",
                directory.display()
            );
        }
    };
    drop(group);
    Ok(RunOutput {
        status,
        stdout,
        stderr,
        directory,
    })
}

fn tail(file: &mut std::fs::File, limit: u64) -> Result<String> {
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(limit)))?;
    let mut bytes = vec![];
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0.and_then(|id| i32::try_from(id).ok()) {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

fn open_log(path: &std::path::Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
