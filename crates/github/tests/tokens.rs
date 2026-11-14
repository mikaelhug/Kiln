use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use kiln_github::{AppKey, Client};

#[derive(Default)]
struct Seen {
    bodies: Vec<serde_json::Value>,
    attempts: usize,
}

async fn access_tokens(
    State(seen): State<Arc<Mutex<Seen>>>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut seen = seen.lock().unwrap();
    seen.attempts += 1;
    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    assert!(bearer.starts_with("Bearer ey"), "app requests are signed with a JWT");
    if seen.attempts == 1 {
        let mut skewed = HeaderMap::new();
        skewed.insert("date", "Thu, 01 Jan 2099 00:00:00 GMT".parse().unwrap());
        return (
            StatusCode::UNAUTHORIZED,
            skewed,
            Json(serde_json::json!({ "message": "'Issued at' claim is in the future" })),
        );
    }
    seen.bodies.push(body);
    (
        StatusCode::CREATED,
        HeaderMap::new(),
        Json(serde_json::json!({ "token": "ghs_scoped", "expires_at": "2099-01-01T01:00:00Z" })),
    )
}

fn test_key() -> Vec<u8> {
    let output = Command::new("openssl")
        .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048"])
        .output()
        .expect("openssl is installed");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    output.stdout
}

#[tokio::test]
async fn build_tokens_are_scoped_to_one_repository_and_survive_clock_skew() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let app =
        Router::new().route("/app/installations/{id}/access_tokens", post(access_tokens)).with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = Client::new(&format!("http://{address}"), AppKey::from_pem(42, &test_key()).unwrap()).unwrap();
    let token = client.scoped_token(7, 1234, &[("contents", "read")]).await.unwrap();

    assert_eq!(token.token, "ghs_scoped");
    assert!(client.now() > 4_000_000_000, "the clock offset from the Date header is applied");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.attempts, 2);
    assert_eq!(
        seen.bodies,
        vec![serde_json::json!({ "repository_ids": [1234], "permissions": { "contents": "read" } })]
    );
}
