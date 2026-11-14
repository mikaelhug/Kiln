# Changelog

All notable changes to Kiln are listed here. Kiln follows [semantic versioning](https://semver.org).

## Unreleased

## 0.1.0

The first public release.

- Builds private GitHub repositories that commit a `.kiln.toml`: tests on pull requests, image builds with BuildKit on default-branch pushes and `v*` tags, smoke tests against the built image, and semver releases to an OCI registry.
- One Kubernetes pod per build with a rootless or user-namespaced BuildKit sidecar, per-repository caches, network policies and an admission policy.
- Release candidates continue above the versions already in the registry; a tag on a verified commit releases that exact image.
- Secrets only reach trusted builds, never pull requests, and are masked in logs. The registry credential only reaches the push step.
- GitHub checks with annotations on the exact `.kiln.toml` line.
- Web UI with GitHub sign-in, live logs with search and line links, build history, release history and registry drift warnings.
- `kiln setup` creates the GitHub App from a manifest; `kiln validate`, `kiln plan` and `kiln schema` check configurations locally; `kiln demo` runs everything against simulated repositories.
