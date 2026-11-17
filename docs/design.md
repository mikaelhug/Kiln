# Design

Kiln is one Rust process with one replica. It runs in a Kubernetes cluster as a GitHub App and keeps its state in SQLite on a `ReadWriteOncePod` volume. The volume access mode is the fence, so there is no leader election. Every loop is level-triggered: after a restart Kiln re-observes its database, the cluster, GitHub and the registry, and carries on.

## Flow

1. **Reconcile**: every minute, and immediately when a verified webhook arrives, Kiln reads the default-branch head, the `v*` tags and the open pull requests of each installed repository, using conditional requests. Webhooks are only hints; their payloads are never trusted. `.kiln.toml` is read through the Contents API at the commit, never from the build workspace.
2. **Schedule**: a fixed number of build slots (default 2) is shared by all repositories, with at most one active build per repository. Tags go first, then default-branch builds, then pull requests. A newer default-branch head or pull-request head supersedes older builds of the same line unless they are already releasing. Tag builds are never cancelled.
3. **Dispatch**: the plan is frozen, the version allocated, and a bare Pod named `kiln-b<build>-a<attempt>` is created. Creation is idempotent.
4. **Observe**: a watcher classifies pod and container states. Step failures, OOM kills, timeouts and configuration errors fail the build. Evictions, node loss and BuildKit restarts are infrastructure errors and are retried.
5. **Logs**: a live follower streams each container to the UI over server-sent events. After a container ends, a final full fetch replaces it unless that fetch fails or is shorter than what was streamed. Secrets are redacted inside the build pod: the step runner masks the secrets its container can see before printing, and image builds run through the same redactor. The server additionally masks GitHub token patterns.
6. **Release**: candidate manifests are copied by digest to the version tag, in configured order. A tag that already points at the same digest is a no-op. A different digest is a hard error.
7. **Report**: GitHub check runs are sent through an outbox, with annotations on the exact `.kiln.toml` line for configuration errors.

## Build pod

Containers start in order, using native sidecars for long-running services:

1. `fetch` downloads the commit tarball with a single-repository, read-only installation token. The token is mounted only in this container and revoked once it finishes.
2. One container per test job runs Kiln's step runner as PID 1. It executes the steps in a shared workspace and prints step markers that Kiln turns into status and timing.
3. A `buildkitd` sidecar (rootless by default) listens on a unix socket, with a per-repository cache volume.
4. One `buildctl` container per image writes an OCI layout. It has no registry credentials.
5. `push` is the only container holding the registry push credential. It pushes the layouts as `sha-<commit>` candidates, which semver image automation ignores.
6. Verify jobs run next to the candidate image as a sidecar.

Pull-request pods only fetch and test. A tag on a commit that was already built and verified on the default branch creates no pod at all; Kiln only releases.

## Versions

- **rc**: `base` is the highest strict `vX.Y.Z` among all git tags, with patch + 1. `N` is one more than the highest of the rc numbers in the registry across all of the repository's images and the database high-water mark. The registry therefore carries the counter, and losing the database is harmless.
- **tags**: a tag gives its version without the `v`.
- **Guard**: a release that would not be the highest semver tag in the registry fails, unless it is a stable hotfix, which is published with a warning.

## Security

- The registry push credential exists only in the Kiln server and in the `push` container. Containers in a pod do not share a process namespace, so a compromised `RUN` step cannot reach it.
- Pull requests never receive secrets: `trust::grant_secrets` allows them only for default-branch pushes, tags reachable from the default branch, and UI runs of the default-branch head.
- Build pods run in their own namespace:
  - no service account token
  - default-deny network policies, with egress to the public internet only
  - rootless BuildKit, and gVisor for pull request builds where available
  - a validating admission policy that confines the one unconfined-seccomp container to the pinned BuildKit image
- Kiln creates its own GitHub App through GitHub's manifest flow. A fresh install only accepts this from someone holding the setup code in its log, and keeps the app's credentials and the registry token in its own database. They are stored unencrypted, because a key on the same volume would protect nothing, so anyone who can read the data volume, its hourly backup or exec into the `kiln` namespace holds them.
- The UI uses GitHub OAuth, limited to the people listed in Settings, with server-side sessions, origin-checked POSTs and a strict CSP.
- Every hour Kiln compares the registry's semver tags with the versions it released and flags any it did not publish. The first scan of an image only records what is already there.
- Kiln never builds itself. Its own image comes from GitHub-hosted Actions on this public repository.
