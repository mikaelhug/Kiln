# Changelog

All notable changes to Kiln are listed here. Kiln follows [semantic versioning](https://semver.org).

## Unreleased

## 0.2.0

- Connect GitHub from the browser: a fresh Kiln asks for the setup code from its log, creates its GitHub App through the manifest flow and keeps the credentials itself. The `kiln setup` command and the GitHub and registry environment variables are gone.
- Settings page: the GitHub App and its repositories, the registry token, and the people who can sign in.
- If nobody can sign in yet, for example after connecting an organization's app from another browser, the first person to sign in claims Kiln with the setup code from its log.
- Upgrading from 0.1: Kiln ignores the old GitHub and registry environment variables and asks to connect a new GitHub App; delete the old app on GitHub afterwards.
- Kiln creates the registry push and pull secrets for build pods itself. Until a token is saved, pull requests still run and other builds ask for one.
- BuildKit always runs rootless with host users, the combination that works on clusters with user namespaces; `KILN_BUILDKIT_ROOTLESS` and `KILN_USER_NAMESPACES` are gone. `KILN_BUILDKIT_IMAGE` and `KILN_BUILDKIT_ARGS` have pinned defaults.

## 0.1.3

- `KILN_SANDBOX_RUNTIME_CLASS` runs pull request builds under a sandboxed runtime class such as gVisor. Builds that run BuildKit keep the default runtime, because BuildKit cannot run inside gVisor.

## 0.1.2

- Kiln starts without a GitHub App and shows a Connect GitHub page until `KILN_APP_ID` is set, so it can be deployed before the app exists.

## 0.1.1

The first public release. Version 0.1.0 was tagged but never published.

- Builds private GitHub repositories that commit a `.kiln.toml`: tests on pull requests, image builds with BuildKit on default-branch pushes and `v*` tags, smoke tests against the built image, and semver releases to an OCI registry.
- One Kubernetes pod per build with a rootless or user-namespaced BuildKit sidecar, per-repository caches, network policies and an admission policy.
- Release candidates continue above the versions already in the registry; a tag on a verified commit releases that exact image.
- Secrets only reach trusted builds, never pull requests, and are masked in logs. The registry credential only reaches the push step.
- GitHub checks with annotations on the exact `.kiln.toml` line.
- Web UI with GitHub sign-in, live logs with search and line links, build history, release history and registry drift warnings.
- `kiln setup` creates the GitHub App from a manifest; `kiln validate`, `kiln plan` and `kiln schema` check configurations locally; `kiln demo` runs everything against simulated repositories.
