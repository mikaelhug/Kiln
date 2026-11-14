use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;

pub struct AppKey {
    app_id: u64,
    key: EncodingKey,
}

#[derive(Serialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: String,
}

impl AppKey {
    pub fn from_pem(app_id: u64, pem: &[u8]) -> Result<Self, jsonwebtoken::errors::Error> {
        Ok(Self { app_id, key: EncodingKey::from_rsa_pem(pem)? })
    }

    pub fn app_id(&self) -> u64 {
        self.app_id
    }

    pub(crate) fn sign(&self, now: i64) -> Result<String, jsonwebtoken::errors::Error> {
        let claims = Claims { iat: now - 60, exp: now + 540, iss: self.app_id.to_string() };
        encode(&Header::new(Algorithm::RS256), &claims, &self.key)
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ManifestOwner {
    pub login: String,
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct CreatedApp {
    pub id: u64,
    pub slug: String,
    pub name: String,
    pub html_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub webhook_secret: Option<String>,
    pub pem: String,
    pub owner: Option<ManifestOwner>,
}

pub async fn convert_manifest(api: &str, code: &str) -> crate::Result<CreatedApp> {
    if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(crate::Error::Shape("the manifest code has unexpected characters".into()));
    }
    let response = reqwest::Client::new()
        .post(format!("{}/app-manifests/{code}/conversions", api.trim_end_matches('/')))
        .header(reqwest::header::USER_AGENT, crate::AGENT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", crate::API_VERSION)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let message = response.text().await.unwrap_or_default();
        return Err(crate::Error::Status { status, message });
    }
    Ok(response.json().await?)
}
