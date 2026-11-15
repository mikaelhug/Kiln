use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{AGENT, Error, Result};

pub struct OAuthApp {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub web: String,
    pub api: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizeRequest {
    pub url: String,
    pub state: String,
    pub verifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct GitHubUser {
    pub id: u64,
    pub login: String,
    pub avatar_url: String,
    pub name: Option<String>,
}

pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

impl OAuthApp {
    pub fn authorize(&self) -> AuthorizeRequest {
        let state = random_token();
        let verifier = random_token();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let url = url::Url::parse_with_params(
            &format!("{}/login/oauth/authorize", self.web.trim_end_matches('/')),
            &[
                ("client_id", self.client_id.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("state", state.as_str()),
                ("code_challenge", challenge.as_str()),
                ("code_challenge_method", "S256"),
                ("allow_signup", "false"),
            ],
        )
        .expect("authorize url is valid")
        .to_string();
        AuthorizeRequest { url, state, verifier }
    }

    pub async fn user(&self, code: &str, verifier: &str) -> Result<GitHubUser> {
        #[derive(Deserialize)]
        struct Exchanged {
            access_token: Option<String>,
            error_description: Option<String>,
        }
        let http = reqwest::Client::new();
        let exchanged: Exchanged = http
            .post(format!("{}/login/oauth/access_token", self.web.trim_end_matches('/')))
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, AGENT)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("code_verifier", verifier),
            ])
            .send()
            .await?
            .json()
            .await?;
        let token = exchanged.access_token.ok_or_else(|| {
            Error::Shape(exchanged.error_description.unwrap_or_else(|| "GitHub returned no access token".into()))
        })?;
        let response = http
            .get(format!("{}/user", self.api.trim_end_matches('/')))
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, AGENT)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await?;
        let user = if response.status().is_success() {
            response.json::<GitHubUser>().await.map_err(Error::from)
        } else {
            Err(Error::Status { status: response.status(), message: response.text().await.unwrap_or_default() })
        };
        let _ = http
            .delete(format!("{}/applications/{}/token", self.api.trim_end_matches('/'), self.client_id))
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .header(ACCEPT, "application/vnd.github+json")
            .header(USER_AGENT, AGENT)
            .json(&serde_json::json!({ "access_token": token }))
            .send()
            .await;
        user
    }
}

pub async fn find_user(api: &str, login: &str) -> Result<GitHubUser> {
    if login.is_empty() || login.len() > 39 || !login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(Error::Shape(format!("{login:?} is not a GitHub username")));
    }
    let response = reqwest::Client::new()
        .get(format!("{}/users/{login}", api.trim_end_matches('/')))
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, AGENT)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Status { status, message: response.text().await.unwrap_or_default() });
    }
    Ok(response.json().await?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenOwner {
    pub login: String,
    pub scopes: Vec<String>,
    pub expires: Option<String>,
}

pub async fn token_owner(api: &str, token: &str) -> Result<TokenOwner> {
    let response = reqwest::Client::new()
        .get(format!("{}/user", api.trim_end_matches('/')))
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, AGENT)
        .header(AUTHORIZATION, format!("token {token}"))
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Status { status, message: response.text().await.unwrap_or_default() });
    }
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let scopes = header("x-oauth-scopes")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let expires = header("github-authentication-token-expiration");
    let user: GitHubUser = response.json().await?;
    Ok(TokenOwner { login: user.login, scopes, expires })
}
