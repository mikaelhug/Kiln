# Kiln

Self-hosted CI for private GitHub repositories on Kubernetes. Kiln watches the repositories a GitHub App is installed on. For every repository that commits a `.kiln.toml`, it runs tests on pull requests, builds images with BuildKit on pushes to the default branch and on `v*` tags, smoke-tests the built image, and publishes semver tags to a container registry for GitOps image automation to deploy.

- [Configuration reference](docs/config.md)
- [Design](docs/design.md)

```bash
cargo install --path crates/kiln
kiln validate
kiln plan --event push --published 0.0.1-rc.55
```
