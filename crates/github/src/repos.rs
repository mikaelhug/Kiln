use reqwest::header::ACCEPT;
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Deserializer};

use crate::{AGENT, Client, Conditional, Error, Result, parse_rfc3339};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token: String,
    pub expires_at: i64,
}

impl<'de> Deserialize<'de> for Token {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            token: String,
            expires_at: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        let expires_at = parse_rfc3339(&raw.expires_at)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid expires_at {:?}", raw.expires_at)))?;
        Ok(Token { token: raw.token, expires_at })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Installation {
    pub id: u64,
    pub account: Account,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Account {
    pub id: u64,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Repository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    pub owner: Account,
    pub private: bool,
    pub archived: bool,
    pub default_branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub sha: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRef {
    pub name: String,
    pub object_sha: String,
    pub annotated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub head_sha: String,
    pub head_ref: String,
    pub base_ref: String,
    pub from_fork: bool,
    pub draft: bool,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    Identical,
    Ahead,
    Behind,
    Diverged,
}

impl Client {
    pub(crate) async fn mint_token(&self, installation: u64, body: &serde_json::Value) -> Result<Token> {
        let path = format!("/app/installations/{installation}/access_tokens");
        Ok(self.as_app(Method::POST, &path, Some(body)).await?.json().await?)
    }

    pub async fn scoped_token(
        &self,
        installation: u64,
        repository_id: u64,
        permissions: &[(&str, &str)],
    ) -> Result<Token> {
        let permissions: serde_json::Map<String, serde_json::Value> =
            permissions.iter().map(|(name, level)| ((*name).to_owned(), (*level).into())).collect();
        let body = serde_json::json!({ "repository_ids": [repository_id], "permissions": permissions });
        self.mint_token(installation, &body).await
    }

    pub async fn revoke(&self, token: &str) -> Result<()> {
        self.send(self.request(Method::DELETE, "/installation/token", &format!("Bearer {token}"))).await?;
        Ok(())
    }

    pub async fn installations(&self) -> Result<Vec<Installation>> {
        Ok(self.as_app(Method::GET, "/app/installations?per_page=100", None).await?.json().await?)
    }

    pub async fn repositories(&self, installation: u64) -> Result<Vec<Repository>> {
        let auth = self.installation_auth(installation).await?;
        self.paginate(&auth, "/installation/repositories?per_page=100", |mut body| {
            match body.get_mut("repositories").map(serde_json::Value::take) {
                Some(serde_json::Value::Array(items)) => items,
                _ => Vec::new(),
            }
        })
        .await
    }

    pub async fn head(
        &self,
        installation: u64,
        repo: &str,
        branch: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Head>> {
        #[derive(Deserialize)]
        struct Branch {
            commit: BranchCommit,
        }
        #[derive(Deserialize)]
        struct BranchCommit {
            sha: String,
            commit: CommitBody,
        }
        #[derive(Deserialize)]
        struct CommitBody {
            message: String,
        }
        let path = format!("/repos/{repo}/branches/{branch}");
        Ok(match self.get_conditional::<Branch>(installation, &path, etag).await? {
            Conditional::NotModified => Conditional::NotModified,
            Conditional::Fresh { value, etag } => {
                Conditional::Fresh { value: Head { sha: value.commit.sha, message: value.commit.commit.message }, etag }
            }
        })
    }

    pub async fn commit_message(&self, installation: u64, repo: &str, sha: &str) -> Result<String> {
        let body: serde_json::Value = self
            .installation_json(installation, Method::GET, &format!("/repos/{repo}/git/commits/{sha}"), None)
            .await?;
        body.get("message")
            .and_then(|m| m.as_str())
            .map(str::to_owned)
            .ok_or_else(|| Error::Shape("commit without message".into()))
    }

    pub async fn version_tags(
        &self,
        installation: u64,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<TagRef>>> {
        #[derive(Deserialize)]
        struct Ref {
            #[serde(rename = "ref")]
            name: String,
            object: Object,
        }
        #[derive(Deserialize)]
        struct Object {
            sha: String,
            #[serde(rename = "type")]
            kind: String,
        }
        let path = format!("/repos/{repo}/git/matching-refs/tags/v?per_page=100");
        Ok(match self.get_conditional_list::<Ref>(installation, &path, etag).await? {
            Conditional::NotModified => Conditional::NotModified,
            Conditional::Fresh { value, etag } => Conditional::Fresh {
                value: value
                    .into_iter()
                    .filter_map(|r| {
                        Some(TagRef {
                            name: r.name.strip_prefix("refs/tags/")?.to_owned(),
                            annotated: r.object.kind == "tag",
                            object_sha: r.object.sha,
                        })
                    })
                    .collect(),
                etag,
            },
        })
    }

    pub async fn tag_commit(&self, installation: u64, repo: &str, tag: &TagRef) -> Result<String> {
        if !tag.annotated {
            return Ok(tag.object_sha.clone());
        }
        let body: serde_json::Value = self
            .installation_json(installation, Method::GET, &format!("/repos/{repo}/git/tags/{}", tag.object_sha), None)
            .await?;
        let object = body.get("object").ok_or_else(|| Error::Shape("tag without object".into()))?;
        let sha = object.get("sha").and_then(|s| s.as_str()).ok_or_else(|| Error::Shape("tag without sha".into()))?;
        if object.get("type").and_then(|t| t.as_str()) == Some("tag") {
            let nested = TagRef { name: tag.name.clone(), object_sha: sha.to_owned(), annotated: true };
            return Box::pin(self.tag_commit(installation, repo, &nested)).await;
        }
        Ok(sha.to_owned())
    }

    pub async fn open_pulls(
        &self,
        installation: u64,
        repo: &str,
        etag: Option<&str>,
    ) -> Result<Conditional<Vec<PullRequest>>> {
        #[derive(Deserialize)]
        struct Pull {
            number: u64,
            title: String,
            draft: Option<bool>,
            head: Side,
            base: Side,
        }
        #[derive(Deserialize)]
        struct Side {
            sha: String,
            #[serde(rename = "ref")]
            name: String,
            repo: Option<SideRepo>,
        }
        #[derive(Deserialize)]
        struct SideRepo {
            id: u64,
        }
        let path = format!("/repos/{repo}/pulls?state=open&per_page=100");
        Ok(match self.get_conditional_list::<Pull>(installation, &path, etag).await? {
            Conditional::NotModified => Conditional::NotModified,
            Conditional::Fresh { value, etag } => Conditional::Fresh {
                value: value
                    .into_iter()
                    .map(|p| PullRequest {
                        number: p.number,
                        from_fork: p.head.repo.as_ref().map(|r| r.id) != p.base.repo.as_ref().map(|r| r.id),
                        head_sha: p.head.sha,
                        head_ref: p.head.name,
                        base_ref: p.base.name,
                        draft: p.draft.unwrap_or(false),
                        title: p.title,
                    })
                    .collect(),
                etag,
            },
        })
    }

    pub async fn file(&self, installation: u64, repo: &str, path: &str, reference: &str) -> Result<Option<String>> {
        let auth = self.installation_auth(installation).await?;
        let request = self
            .request(Method::GET, &format!("/repos/{repo}/contents/{path}"), &auth)
            .query(&[("ref", reference)])
            .header(ACCEPT, "application/vnd.github.raw+json");
        match self.send(request).await {
            Ok(response) => Ok(Some(response.text().await?)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub async fn compare(&self, installation: u64, repo: &str, base: &str, head: &str) -> Result<Compare> {
        let body: serde_json::Value = self
            .installation_json(
                installation,
                Method::GET,
                &format!("/repos/{repo}/compare/{base}...{head}?per_page=1"),
                None,
            )
            .await?;
        match body.get("status").and_then(|s| s.as_str()) {
            Some("identical") => Ok(Compare::Identical),
            Some("ahead") => Ok(Compare::Ahead),
            Some("behind") => Ok(Compare::Behind),
            Some("diverged") => Ok(Compare::Diverged),
            other => Err(Error::Shape(format!("unknown compare status {other:?}"))),
        }
    }
}

pub async fn download_tarball(api: &str, token: &str, repo: &str, sha: &str) -> Result<reqwest::Response> {
    let response = reqwest::Client::new()
        .get(format!("{}/repos/{repo}/tarball/{sha}", api.trim_end_matches('/')))
        .header(reqwest::header::USER_AGENT, AGENT)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await?;
    if response.status() == StatusCode::OK {
        Ok(response)
    } else {
        Err(Error::Status { status: response.status(), message: response.text().await.unwrap_or_default() })
    }
}
