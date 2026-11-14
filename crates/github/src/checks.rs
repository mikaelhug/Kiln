use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::{Client, Result};

const ANNOTATIONS_PER_REQUEST: usize = 50;
const MAX_TEXT: usize = 65_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Queued,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    Skipped,
    TimedOut,
    ActionRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Annotation {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub annotation_level: &'static str,
    pub message: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CheckOutput {
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckRun {
    pub name: String,
    pub head_sha: String,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<CheckConclusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<CheckOutput>,
}

#[derive(Deserialize)]
struct Created {
    id: u64,
}

impl Client {
    pub async fn create_check(&self, installation: u64, repo: &str, run: &CheckRun) -> Result<u64> {
        let (first, rest) = split(run);
        let created: Created = self
            .installation_json(installation, Method::POST, &format!("/repos/{repo}/check-runs"), Some(&first))
            .await?;
        self.append_annotations(installation, repo, created.id, run, rest).await?;
        Ok(created.id)
    }

    pub async fn update_check(&self, installation: u64, repo: &str, id: u64, run: &CheckRun) -> Result<()> {
        let (first, rest) = split(run);
        let path = format!("/repos/{repo}/check-runs/{id}");
        let _: serde_json::Value = self.installation_json(installation, Method::PATCH, &path, Some(&first)).await?;
        self.append_annotations(installation, repo, id, run, rest).await
    }

    async fn append_annotations(
        &self,
        installation: u64,
        repo: &str,
        id: u64,
        run: &CheckRun,
        rest: Vec<Vec<Annotation>>,
    ) -> Result<()> {
        let Some(output) = &run.output else { return Ok(()) };
        for chunk in rest {
            let body = serde_json::json!({
                "output": { "title": output.title, "summary": output.summary, "annotations": chunk }
            });
            let path = format!("/repos/{repo}/check-runs/{id}");
            let _: serde_json::Value = self.installation_json(installation, Method::PATCH, &path, Some(&body)).await?;
        }
        Ok(())
    }
}

fn split(run: &CheckRun) -> (serde_json::Value, Vec<Vec<Annotation>>) {
    let mut first = run.clone();
    let mut rest = Vec::new();
    if let Some(output) = first.output.as_mut() {
        output.summary = clamp(&output.summary);
        output.text = output.text.as_deref().map(clamp);
        let mut chunks: Vec<Vec<Annotation>> =
            output.annotations.chunks(ANNOTATIONS_PER_REQUEST).map(<[Annotation]>::to_vec).collect();
        output.annotations = if chunks.is_empty() { Vec::new() } else { chunks.remove(0) };
        rest = chunks;
    }
    (serde_json::to_value(first).expect("check run serializes"), rest)
}

fn clamp(text: &str) -> String {
    if text.len() <= MAX_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n\n…truncated", &text[..end])
}
