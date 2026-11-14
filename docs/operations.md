# Operating Kiln

## 1. Create the GitHub App

In **Settings › Developer settings › GitHub Apps › New GitHub App**:

| Field | Value |
|---|---|
| Homepage URL | `https://kiln.example.com` |
| Callback URL | `https://kiln.example.com/auth/callback` |
| Expire user authorization tokens | on |
| Request user authorization during installation | off |
| Webhook URL | `https://kiln.example.com/hooks/github` |
| Webhook secret | a long random string (`openssl rand -hex 32`) |
| Repository permissions | Contents: read · Checks: read and write · Pull requests: read · Metadata: read |
| Subscribe to events | Push · Pull request · Check run · Create · Installation repositories · Repository |
| Where can this app be installed | Only on this account |

Then:
1. Generate a private key.
2. Note the App ID and the Client ID, and generate a client secret.
3. Install the app on **selected repositories** only.

Adding a repository to the installation and committing a `.kiln.toml` is all a repository needs.

Webhooks are only hints. Kiln polls every repository with conditional requests, so it keeps working when a webhook is lost or the app is temporarily unreachable.

## 2. Registry credential

Kiln pushes candidates and releases with one credential. For GitHub Container Registry, use a classic personal access token with only `write:packages`. A release tag deploys straight to production, so this credential is the most sensitive secret Kiln holds:
- **Where it lives:** only the Kiln server and the `push` container of a build ever see it.
- **Expiry:** give it a one-year expiry and rotate it before that.

Nodes also need a pull secret (`KILN_PULL_SECRET`) in the build namespace so verify jobs can start the private candidate image.

## 3. Prepare the cluster

Builds run in `kiln-builds`. Each build pod runs a rootless BuildKit daemon as a sidecar.

**User namespaces.** Rootless BuildKit and `hostUsers: false` need user namespaces. Some distributions turn them off; on Talos, set the sysctl `user.max_user_namespaces` (for example `11255`) on every node. Without them, set `KILN_USER_NAMESPACES=false` and expect weaker isolation of test containers.

**BuildKit mode.** Kiln supports two ways to run BuildKit, chosen with `KILN_BUILDKIT_ROOTLESS`:

- `true` (default): the rootless image as uid 1000. rootlesskit needs `newuidmap`, a subuid range inside the pod's user namespace, and privilege escalation for that one container.
- `false`: the regular image as uid 0. With `KILN_USER_NAMESPACES=true` that uid 0 is unprivileged on the node, which is often the simpler choice on clusters with user namespaces.

Pin the image by digest (`moby/buildkit:<version>-rootless` or `moby/buildkit:<version>`) and start with `KILN_BUILDKIT_ARGS=--oci-worker-no-process-sandbox`. Verify on your cluster that:
1. a build of a simple Dockerfile works;
2. a second build reuses the cache;
3. builds finish at your pod network MTU. Kiln runs BuildKit with `--oci-worker-net=host`, so `RUN` steps inherit the pod's MTU.

**Cache storage.** Each repository gets a `ReadWriteOnce` claim `kiln-cache-<owner>-<repo>`, created on demand (`KILN_CACHE_SIZE`, `KILN_CACHE_STORAGE_CLASS`). A build only ever mounts its own repository's cache. Pull requests use a separate directory in it, so they cannot poison the default branch's cache.

**Network.** The example policies allow build pods to reach DNS and the internet on ports 80 and 443, and nothing else. That blocks the LAN, the cluster network and the Kubernetes API.

The server needs the Kubernetes API, which network policies match after DNAT. Allow both the service address (`10.96.0.1:443`) and the API server's real address and port; replace `192.0.2.10:6443` in `network-policies.yaml`.

## 4. Deploy

`deploy/kubernetes` is a kustomization with everything Kiln needs:
- the namespaces and their Pod Security levels;
- RBAC that lets Kiln manage pods, pod logs, fetch-token secrets and cache claims in `kiln-builds`. Kiln can create and delete secrets there but never read them;
- a ValidatingAdmissionPolicy that refuses build pods with a service account token, host namespaces, host paths, privileged containers or added capabilities. It allows unconfined seccomp only for the pinned `buildkitd` container;
- the network policies, the server, its storage and its route.

Copy it into your GitOps repository and replace every `example` value. Keep secrets encrypted, for example with sops.

The server runs as a single replica on a `ReadWriteOncePod` volume, so two Kiln processes can never share the database.

## 5. Onboard a repository

1. Add the repository to the GitHub App installation.
2. Commit a `.kiln.toml` with `[release] shadow = true`. Kiln runs everything except the release and reports the version it *would* release.
3. When the shadow runs look right, remove `shadow = true` and delete the old CI workflow in the same commit.

Release candidates continue above the highest `-rc.N` already in the registry, so image automation keeps working when you switch from another CI.

**Repository secrets** live in a Secret named `kiln-repo-<owner>-<repo>` (lowercase, `/` replaced by `-`) in `kiln-builds`. Reference keys from `[[images]].secrets` or a job's `secrets`. Only default-branch pushes, tags reachable from the default branch, and builds started from the UI receive them; pull requests never do.

## 6. Configuration reference

Values marked *file* can also be given as `<NAME>_FILE` pointing at a mounted secret.

| Variable | Default | |
|---|---|---|
| `KILN_PUBLIC_URL` | required | https URL of the UI, used for links and the OAuth callback |
| `KILN_APP_ID` | required | GitHub App ID |
| `KILN_APP_PRIVATE_KEY` | required, *file* | GitHub App private key (PEM) |
| `KILN_WEBHOOK_SECRET` | required, *file* | webhook secret |
| `KILN_WEBHOOK_SECRET_PREVIOUS` | *file* | the previous webhook secret while rotating |
| `KILN_OAUTH_CLIENT_ID` | required | GitHub App client ID |
| `KILN_OAUTH_CLIENT_SECRET` | required, *file* | GitHub App client secret |
| `KILN_ALLOWED_USER_IDS` | required | numeric GitHub user IDs allowed to sign in, comma-separated |
| `KILN_OWNERS` | required | accounts whose repositories Kiln builds |
| `KILN_INCLUDE_PUBLIC` | `false` | also build public repositories |
| `KILN_REGISTRY` | required | e.g. `ghcr.io/example` |
| `KILN_REGISTRY_URL` | `https://<registry host>` | |
| `KILN_REGISTRY_USERNAME` | `kiln` | |
| `KILN_REGISTRY_PASSWORD` | required, *file* | push credential |
| `KILN_IMAGE` | required | Kiln's own image, pinned by digest, used for the in-pod helpers |
| `KILN_BUILDKIT_IMAGE` | required | pinned BuildKit image |
| `KILN_BUILDKIT_ARGS` | | extra `buildkitd` flags, comma-separated |
| `KILN_BUILDKIT_ROOTLESS` | `true` | run BuildKit as uid 1000 with the rootless image, or as uid 0 inside the pod's user namespace |
| `KILN_BUILD_NAMESPACE` | `kiln-builds` | |
| `KILN_PULL_SECRET` | | image pull secret in the build namespace |
| `KILN_PUSH_SECRET` | `kiln-registry-push` | Secret with `username` and `password` used by the push container |
| `KILN_PRIORITY_CLASS` | | PriorityClass for build pods |
| `KILN_USER_NAMESPACES` | `true` | run build pods with `hostUsers: false` |
| `KILN_AVOID_POD_LABELS` / `KILN_AVOID_POD_NAMESPACE` | | keep builds off the node running these pods, e.g. the Kiln server |
| `KILN_SLOTS` | `2` | builds that run at the same time |
| `KILN_POLL_SECONDS` | `60` | how often Kiln checks each repository |
| `KILN_BUILD_TIMEOUT_SECONDS` | `1800` | per build, at most 2 hours |
| `KILN_MAX_ATTEMPTS` / `KILN_RETRY_SECONDS` | `3` / `30` | retries after infrastructure failures, with exponential backoff |
| `KILN_DEFAULT_CPU` / `KILN_DEFAULT_MEMORY` / `KILN_MEMORY_LIMIT_FACTOR` | `1` / `2Gi` / `2` | job resources when a job sets none; the memory limit is the request times the factor |
| `KILN_WORKSPACE_SIZE` / `KILN_OUTPUT_SIZE` | `8Gi` / `10Gi` | per-build scratch space |
| `KILN_CACHE_SIZE` / `KILN_CACHE_STORAGE_CLASS` | `20Gi` / default class | per-repository cache |
| `KILN_REPO_SECRET_PREFIX` | `kiln-repo-` | |
| `KILN_DATA_DIR` | `/data` | database, logs and the hourly backup |
| `KILN_LISTEN` | `0.0.0.0:8080` | |

## 7. Runbook

| Situation | What to do |
|---|---|
| A build fails with a strange cache error | **Reset cache** on the repository page. The next build starts cold. |
| Kiln restarted during a build | Nothing. Kiln re-attaches to running pods, re-reads their logs and resumes releases. |
| A node was drained during a build | Kiln retries the build on another node, up to `KILN_MAX_ATTEMPTS`. |
| GitHub is down | Builds wait. Kiln catches up from polling, and check runs are retried from an outbox. |
| The database is lost | Delete the claim and restart. Version numbers continue from the registry; only history is lost. |
| A release failed with "semver tags are immutable" | Someone or something already published that version with a different image. Release a new version. |
| Rotate the webhook secret | Set the new value and keep the old one in `KILN_WEBHOOK_SECRET_PREVIOUS` until GitHub uses the new one. |
| Rotate the App key or the client secret | GitHub allows two at a time: add the new one, deploy, then delete the old one. |
| Stop all builds for a repository | **Pause** on the repository page. |
| A repository shows "Published outside Kiln" | Every hour Kiln compares the registry's semver tags with what it released. Another CI still publishing, or someone using the registry credential, shows up here. Find the source, then **Dismiss**. |

Kiln writes a copy of its database to `kiln.backup.db` in the data directory every hour.

## 8. Try it without a cluster

```bash
kiln demo --port 8080
```

The demo runs the real engine and UI against simulated repositories, builds and logs, on `127.0.0.1`. It starts with a few days of build history and pushes a new commit every minute or so.
