use std::path::Path;
use std::process::Command;

use kiln_registry::{Error, OCI_INDEX, OCI_MANIFEST, Promotion, Registry, digest_of};

struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
    }
}

fn start_registry() -> (Container, String) {
    let output = Command::new("docker")
        .args(["run", "-d", "-p", "127.0.0.1::5000", "registry:3"])
        .output()
        .expect("docker is installed");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let id = String::from_utf8(output.stdout).unwrap().trim().to_owned();
    let container = Container(id.clone());
    let port = Command::new("docker").args(["port", &id, "5000/tcp"]).output().unwrap();
    let address = String::from_utf8(port.stdout).unwrap().lines().next().unwrap().trim().to_owned();
    (container, format!("http://{address}"))
}

async fn wait_ready(base: &str) {
    for _ in 0..100 {
        if reqwest::get(format!("{base}/v2/")).await.is_ok_and(|r| r.status().is_success()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("registry did not start");
}

fn write_blob(layout: &Path, bytes: &[u8]) -> serde_json::Value {
    let digest = digest_of(bytes);
    let dir = layout.join("blobs/sha256");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(digest.trim_start_matches("sha256:")), bytes).unwrap();
    serde_json::json!({ "digest": digest, "size": bytes.len() })
}

fn image_layout(layout: &Path, content: &[u8]) -> String {
    std::fs::write(layout.join("oci-layout"), r#"{"imageLayoutVersion":"1.0.0"}"#).unwrap();
    let config =
        write_blob(layout, br#"{"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#);
    let layer = write_blob(layout, content);
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_MANIFEST,
        "config": { "mediaType": "application/vnd.oci.image.config.v1+json", "digest": config["digest"], "size": config["size"] },
        "layers": [{ "mediaType": "application/vnd.oci.image.layer.v1.tar", "digest": layer["digest"], "size": layer["size"] }],
    });
    let manifest = write_blob(layout, &serde_json::to_vec(&manifest).unwrap());
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": OCI_INDEX,
        "manifests": [{ "mediaType": OCI_MANIFEST, "digest": manifest["digest"], "size": manifest["size"],
                        "platform": { "architecture": "amd64", "os": "linux" } }],
    });
    let index = write_blob(layout, &serde_json::to_vec(&index).unwrap());
    let top = serde_json::json!({
        "schemaVersion": 2,
        "manifests": [{ "mediaType": OCI_INDEX, "digest": index["digest"], "size": index["size"] }],
    });
    std::fs::write(layout.join("index.json"), serde_json::to_vec(&top).unwrap()).unwrap();
    index["digest"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn candidates_are_pushed_and_promoted_without_ever_moving_a_semver_tag() {
    let (_container, base) = start_registry();
    wait_ready(&base).await;
    let registry = Registry::new(&base, None).unwrap();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let candidate = image_layout(first.path(), b"layer one");
    let other = image_layout(second.path(), b"layer two");

    let pushed = registry.push_layout("o/app", first.path(), "sha-aaaa").await.unwrap();
    assert_eq!(pushed, candidate);
    assert_eq!(registry.push_layout("o/app", first.path(), "sha-aaaa").await.unwrap(), candidate);
    registry.push_layout("o/app", second.path(), "sha-bbbb").await.unwrap();

    assert_eq!(registry.promote("o/app", &candidate, "0.0.1-rc.56").await.unwrap(), Promotion::Created);
    assert_eq!(registry.promote("o/app", &candidate, "0.0.1-rc.56").await.unwrap(), Promotion::AlreadyThere);
    let moved = registry.promote("o/app", &other, "0.0.1-rc.56").await;
    assert!(matches!(moved, Err(Error::TagTaken { .. })), "{moved:?}");

    let released = registry.manifest("o/app", "0.0.1-rc.56").await.unwrap().unwrap();
    assert_eq!(released.digest, candidate);
    let mut tags = registry.tags("o/app").await.unwrap();
    tags.sort();
    assert_eq!(tags, ["0.0.1-rc.56", "sha-aaaa", "sha-bbbb"]);
    assert!(registry.tags("o/missing").await.unwrap().is_empty());
}
