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
