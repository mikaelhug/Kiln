use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use anyhow::Result;
use kiln_core::redact::Redactor;
use serde::Serialize;
use tokio::sync::broadcast;

pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Build { build: i64, repo: i64 },
    Repo { repo: i64 },
    Log { build: i64, attempt: i64, task: i64, offset: u64, text: String },
    LogReset { build: i64, attempt: i64, task: i64 },
}

pub struct Logs {
    dir: PathBuf,
    redactor: Redactor,
    events: broadcast::Sender<Event>,
}

impl Logs {
    pub fn new(dir: PathBuf, events: broadcast::Sender<Event>) -> Self {
        Self { dir, redactor: Redactor::new::<&str>(&[]), events }
    }

    pub fn events(&self) -> &broadcast::Sender<Event> {
        &self.events
    }

    fn path(&self, build: i64, attempt: i64, task: i64) -> PathBuf {
        self.dir.join("logs").join(build.to_string()).join(attempt.to_string()).join(format!("{task}.log"))
    }

    fn clean(&self, text: &str) -> String {
        text.lines().map(|line| self.redactor.redact(line)).collect::<Vec<_>>().join("\n")
            + if text.ends_with('\n') { "\n" } else { "" }
    }

    pub fn append(&self, build: i64, attempt: i64, task: i64, chunk: &str) -> Result<()> {
        let path = self.path(build, attempt, task);
        std::fs::create_dir_all(path.parent().expect("log path has a parent"))?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
        let offset = file.metadata()?.len();
        if offset >= MAX_LOG_BYTES {
            return Ok(());
        }
        let mut text = self.clean(chunk);
        let room = (MAX_LOG_BYTES - offset) as usize;
        if text.len() > room {
            let mut end = room;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n… Kiln stopped recording this log at 10 MB; the downloaded log keeps the end …\n");
        }
        file.write_all(text.as_bytes())?;
        let _ = self.events.send(Event::Log { build, attempt, task, offset, text });
        Ok(())
    }

    pub fn replace(&self, build: i64, attempt: i64, task: i64, text: &str) -> Result<u64> {
        let path = self.path(build, attempt, task);
        std::fs::create_dir_all(path.parent().expect("log path has a parent"))?;
        let mut text = self.clean(text);
        if text.len() as u64 > MAX_LOG_BYTES {
            let half = (MAX_LOG_BYTES / 2) as usize;
            let mut head_end = half;
            while !text.is_char_boundary(head_end) {
                head_end -= 1;
            }
            let mut tail_start = text.len() - half;
            while !text.is_char_boundary(tail_start) {
                tail_start += 1;
            }
            let skipped = tail_start - head_end;
            text = format!("{}\n… {skipped} bytes of log omitted …\n{}", &text[..head_end], &text[tail_start..]);
        }
        let previous = std::fs::read_to_string(&path).unwrap_or_default();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &text)?;
        std::fs::rename(&tmp, &path)?;
        if previous != text {
            let _ = self.events.send(Event::LogReset { build, attempt, task });
        }
        Ok(text.len() as u64)
    }

    pub fn reset(&self, build: i64, attempt: i64, task: i64) -> Result<()> {
        let path = self.path(build, attempt, task);
        std::fs::create_dir_all(path.parent().expect("log path has a parent"))?;
        std::fs::write(&path, "")?;
        let _ = self.events.send(Event::LogReset { build, attempt, task });
        Ok(())
    }

    pub fn read(&self, build: i64, attempt: i64, task: i64, from: u64) -> String {
        let Ok(mut file) = std::fs::File::open(self.path(build, attempt, task)) else { return String::new() };
        if file.seek(SeekFrom::Start(from)).is_err() {
            return String::new();
        }
        let mut bytes = Vec::new();
        let _ = file.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn remove_build(&self, build: i64) {
        let _ = std::fs::remove_dir_all(self.dir.join("logs").join(build.to_string()));
    }
}
