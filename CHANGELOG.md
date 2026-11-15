# Changelog

All notable changes to Kiln are listed here. Kiln follows [semantic versioning](https://semver.org).

## Unreleased

- Image builds that pull a base image from a registry no longer fail with "mkdir /home/user/.docker: read-only file system".
- `kiln builds` and `kiln logs [--follow]` read builds and their redacted logs inside the server pod, so people and agents can follow a build through `kubectl exec`.
- Build pods start: the image keeps its binary at `/usr/local/bin/kiln`, so the helper's `/kiln/bin` mount no longer collides with it.
- A container that cannot start shows the runtime's reason instead of "exited with code 128", and its duration no longer counts from 1970.
- Live updates cover every page: steps update as they start and finish, releases show each image as it is published, System and Settings follow changes, and a browser that misses events catches up instead of going stale.
- Every push to `main` publishes a tested `X.Y.(Z+1)-rc.N` image, so image automation can follow the latest Kiln.
- `KILN_IMAGE` is optional: a release knows its own image, so image automation only updates the server image.
- A service that stops or never opens its `port` fails the build instead of waiting for the timeout.
- `services.port` must be 1 to 65535.
- Without a registry token, default-branch builds wait and start once one is saved; pull requests still run.
- **Settings › Registry** guides through creating the token, checks it with GitHub on save, takes the username from it and shows when it expires. Every page reminds you when no token is saved or it expires within two weeks.
- A registry that refuses the token, Kubernetes refusing a build pod, and a tag outside the default branch each show their own reason instead of ".kiln.toml has errors", and are no longer logged as warnings.

## 0.2.1

- Live updates stop within seconds for someone who is removed or signed out.
- **Settings › GitHub › Disconnect** recovers from a deleted or revoked GitHub App without losing people or history.
- Settings and System show when Kiln could not create the registry secrets for build pods.
- Adding a person says when GitHub did not answer instead of claiming the user does not exist.
- `[[images]].resources` gives BuildKit more CPU and memory for heavy builds.
- `services[].port` makes a job wait until its service, such as Postgres, accepts connections.

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
