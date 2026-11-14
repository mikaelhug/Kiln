# Kiln

Self-hosted CI for private GitHub repositories on Kubernetes.

Kiln watches the repositories its GitHub App is installed on. For every repository that commits a `.kiln.toml` it:
- runs the tests on pull requests;
- builds images with BuildKit on pushes to the default branch and on `v*` tags;
- smoke-tests the freshly built image;
- publishes a semver tag to your registry for GitOps image automation to deploy.

Results show up as GitHub checks and live in Kiln's web UI.

```toml
version = 1

[[images]]
name = "web"

[test.unit]
image = "node:26"
steps = ["npm ci", "npm test"]
```

- **Two build slots shared by every repository.** Newer pushes supersede older builds, and tags always build.
- **Releases continue your existing versions.** A push releases `X.Y.(Z+1)-rc.N` above anything already published. A tag on a tested commit releases that exact image.
- **Credentials stay contained.** The registry credential never reaches BuildKit, tests or pull requests. Secrets are masked in logs, and build pods are locked down by network policies and an admission policy.

## Documentation

- [Configuration reference](docs/config.md)
- [Operating Kiln](docs/operations.md): GitHub App, cluster, deployment, onboarding, runbook
- [Design](docs/design.md)
- [Kubernetes deployment kit](deploy/kubernetes)

## Try it

```bash
cargo run -p kiln -- demo
```

`kiln demo` serves the real UI on <http://localhost:8080> with simulated repositories and builds. Nothing outside your machine is contacted.

```bash
kiln validate
kiln plan --event push --published 0.0.1-rc.55
kiln schema > kiln.schema.json
```

## Develop

```bash
just check
```

`just check` runs formatting, clippy, the no-comments rule, the tests and the schema drift check. The registry tests start a `registry:3` container, so Docker must be running.
