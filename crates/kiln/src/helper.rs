use std::io::{BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use futures::StreamExt;
use kiln_core::redact::Redactor;
use kiln_registry::Registry;
use kiln_runner::{Marker, StepSpec, render_marker};
use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::signal::unix::{SignalKind, signal};

#[derive(Subcommand)]
pub enum Helper {
    Fetch {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        sha: String,
        #[arg(long)]
        dest: PathBuf,
        #[arg(long)]
        install: Option<PathBuf>,
        #[arg(long)]
        api: String,
        #[arg(long)]
        token_file: PathBuf,
    },
    Steps,
    Exec {
        #[arg(long)]
        redact_dir: Option<PathBuf>,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    Push {
        #[arg(long)]
        registry: String,
        #[arg(long)]
        tag: String,
        #[arg(long = "image")]
        images: Vec<String>,
    },
    Done,
}

pub async fn run(helper: Helper) -> Result<ExitCode> {
    match helper {
        Helper::Fetch { repo, sha, dest, install, api, token_file } => {
            fetch(&repo, &sha, &dest, install.as_deref(), &api, &token_file).await?;
            Ok(ExitCode::SUCCESS)
        }
        Helper::Steps => steps().await,
        Helper::Exec { redact_dir, command } => exec(redact_dir.as_deref(), &command).await,
        Helper::Push { registry, tag, images } => {
            push(&registry, &tag, &images).await?;
            Ok(ExitCode::SUCCESS)
        }
        Helper::Done => Ok(ExitCode::SUCCESS),
    }
}

async fn retry<T, F, Fut, E>(what: &str, mut attempt: F, transient: impl Fn(&E) -> bool) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display + Send + Sync + 'static,
{
    let mut delay = Duration::from_secs(2);
    for tries in 1.. {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) if tries < 4 && transient(&error) => {
                println!("{what} failed ({error}), retrying in {}s", delay.as_secs());
                tokio::time::sleep(delay).await;
                delay *= 3;
            }
            Err(error) => bail!("{what} failed: {error}"),
        }
    }
    unreachable!("the retry loop only ends by returning")
}

async fn fetch(repo: &str, sha: &str, dest: &Path, install: Option<&Path>, api: &str, token_file: &Path) -> Result<()> {
    if let Some(dir) = install {
        let target = dir.join("kiln");
        std::fs::copy(std::env::current_exe()?, &target)
            .with_context(|| format!("installing kiln into {}", dir.display()))?;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    }
    let token = std::fs::read_to_string(token_file).context("reading the fetch token")?.trim().to_owned();
    println!("Fetching {repo} at {sha}");
    let started = Instant::now();
    let archive = dest.join(".kiln-source.tar.gz");
    let response = retry(
        "downloading the source",
        || kiln_github::download_tarball(api, &token, repo, sha),
        kiln_github::Error::is_transient,
    )
    .await?;
    let mut file = tokio::fs::File::create(&archive).await?;
    let mut stream = response.bytes_stream();
    let mut bytes = 0u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        bytes += chunk.len() as u64;
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    let dest_owned = dest.to_owned();
    let files = tokio::task::spawn_blocking(move || extract(&archive, &dest_owned)).await??;
    println!("Fetched {files} files ({:.1} MB) in {:.1}s", bytes as f64 / 1_000_000.0, started.elapsed().as_secs_f64());
    Ok(())
}

fn extract(archive: &Path, dest: &Path) -> Result<usize> {
    let staging = dest.join(".kiln-extract");
    std::fs::create_dir_all(&staging)?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
    tar.set_preserve_permissions(true);
    let mut files = 0;
    for entry in tar.entries()? {
        let mut entry = entry?;
        if matches!(entry.header().entry_type(), tar::EntryType::XGlobalHeader | tar::EntryType::XHeader) {
            continue;
        }
        if !entry.unpack_in(&staging)? {
            bail!("the source archive contains an unsafe path: {}", entry.path()?.display());
        }
        if entry.header().entry_type().is_file() {
            files += 1;
        }
    }
    std::fs::remove_file(archive)?;
    let mut roots = std::fs::read_dir(&staging)?.collect::<std::io::Result<Vec<_>>>()?;
    let [root] = roots.as_mut_slice() else {
        bail!("the source archive has {} top-level entries, expected 1", roots.len())
    };
    for child in std::fs::read_dir(root.path())? {
        let child = child?;
        std::fs::rename(child.path(), dest.join(child.file_name()))?;
    }
    std::fs::remove_dir_all(&staging)?;
    Ok(files)
}

fn emit(marker: &Marker) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", render_marker(marker));
    let _ = out.flush();
}

fn unix_now() -> i64 {
    kiln_github::unix_now()
}

async fn steps() -> Result<ExitCode> {
    let specs: Vec<Value> = serde_json::from_str(&std::env::var("KILN_STEPS").context("KILN_STEPS is not set")?)?;
    let specs: Vec<StepSpec> = specs
        .into_iter()
        .map(|s| StepSpec {
            name: s["name"].as_str().unwrap_or_default().to_owned(),
            run: s["run"].as_str().unwrap_or_default().to_owned(),
        })
        .collect();
    let timeout: u64 = std::env::var("KILN_TIMEOUT").ok().and_then(|t| t.parse().ok()).unwrap_or(1800);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
    let bash = Path::new("/bin/bash").exists();
    let redactor = Arc::new(redactor(None)?);
    let mut terminate = signal(SignalKind::terminate())?;
    for (index, step) in specs.iter().enumerate() {
        emit(&Marker::StepStart { index, at: unix_now() });
        for line in step.run.lines() {
            println!("$ {}", redactor.redact(line));
        }
        let command = if bash {
            let mut c = std::process::Command::new("/bin/bash");
            c.args(["-eo", "pipefail", "-c", &step.run]);
            c
        } else {
            let mut c = std::process::Command::new("/bin/sh");
            c.args(["-ec", &step.run]);
            c
        };
        let code = supervise(command, &redactor, deadline, timeout, &mut terminate).await?;
        emit(&Marker::StepEnd { index, exit: code, at: unix_now() });
        if code != 0 {
            return Ok(exit_code(code));
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn exec(redact_dir: Option<&Path>, command: &[String]) -> Result<ExitCode> {
    let [program, args @ ..] = command else { bail!("exec needs a command") };
    let redactor = Arc::new(redactor(redact_dir)?);
    let mut terminate = signal(SignalKind::terminate())?;
    let mut child = std::process::Command::new(program);
    child.args(args);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(24 * 3600);
    let code = supervise(child, &redactor, deadline, 24 * 3600, &mut terminate).await?;
    Ok(exit_code(code))
}

fn exit_code(code: i32) -> ExitCode {
    if code == 0 { ExitCode::SUCCESS } else { ExitCode::from(u8::try_from(code).unwrap_or(1).max(1)) }
}

fn redactor(dir: Option<&Path>) -> Result<Redactor> {
    let mut secrets: Vec<String> = std::env::var("KILN_SECRET_NAMES")
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .filter_map(|name| std::env::var(name).ok())
        .collect();
    if let Some(dir) = dir
        && dir.exists()
    {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_file()
                && let Ok(value) = std::fs::read_to_string(&path)
            {
                secrets.push(value.trim_end_matches('\n').to_owned());
            }
        }
    }
    Ok(Redactor::new(&secrets))
}

async fn supervise(
    mut command: std::process::Command,
    redactor: &Arc<Redactor>,
    deadline: tokio::time::Instant,
    timeout: u64,
    terminate: &mut tokio::signal::unix::Signal,
) -> Result<i32> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .with_context(|| format!("starting {:?}", command.get_program()))?;
    let pid = Pid::from_raw(child.id() as i32);
    let readers: Vec<_> = [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let redactor = redactor.clone();
        std::thread::spawn(move || relay(stream, &redactor))
    })
    .collect();
    let mut waiter = tokio::task::spawn_blocking(move || wait_for(pid));
    let code = tokio::select! {
        code = &mut waiter => code?,
        _ = tokio::time::sleep_until(deadline) => {
            println!("Kiln: the job ran past its {timeout}s timeout and was stopped");
            stop(pid, &mut waiter).await?;
            124
        }
        _ = terminate.recv() => {
            println!("Kiln: the build was cancelled");
            stop(pid, &mut waiter).await?;
            143
        }
    };
    for reader in readers {
        let _ = reader.join();
    }
    Ok(code)
}

fn relay(stream: Box<dyn Read + Send>, redactor: &Redactor) {
    let mut reader = std::io::BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.trim_end_matches(['\n', '\r']);
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{}", redactor.redact(text));
                let _ = out.flush();
            }
        }
    }
}

fn wait_for(pid: Pid) -> i32 {
    loop {
        match waitpid(None, None) {
            Ok(WaitStatus::Exited(child, code)) if child == pid => return code,
            Ok(WaitStatus::Signaled(child, signal, _)) if child == pid => return 128 + signal as i32,
            Ok(_) | Err(Errno::EINTR) => continue,
            Err(_) => return 1,
        }
    }
}

async fn stop(pid: Pid, waiter: &mut tokio::task::JoinHandle<i32>) -> Result<()> {
    let _ = killpg(pid, Signal::SIGTERM);
    if tokio::time::timeout(Duration::from_secs(5), &mut *waiter).await.is_err() {
        let _ = killpg(pid, Signal::SIGKILL);
        waiter.await?;
    }
    Ok(())
}

async fn push(url: &str, tag: &str, images: &[String]) -> Result<()> {
    let credentials = match (std::env::var("KILN_REGISTRY_USERNAME"), std::env::var("KILN_REGISTRY_PASSWORD")) {
        (Ok(user), Ok(password)) if !password.is_empty() => Some((user, password)),
        _ => None,
    };
    let registry = Registry::new(url, credentials)?;
    for image in images {
        let mut parts = image.splitn(3, '=');
        let (Some(name), Some(repository), Some(dir)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("invalid --image {image:?}, expected name=repository=dir");
        };
        println!("Pushing {name} to {}/{repository}:{tag}", registry.host());
        let started = Instant::now();
        let digest = retry(
            &format!("pushing {name}"),
            || registry.push_layout(repository, Path::new(dir), tag),
            kiln_registry::Error::is_transient,
        )
        .await?;
        println!("Pushed {name} as {digest} in {:.1}s", started.elapsed().as_secs_f64());
        emit(&Marker::Image { name: name.to_owned(), digest });
    }
    Ok(())
}
