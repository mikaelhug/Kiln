# Operating Kiln

This guide takes you from nothing to a first build. Kiln ships as one image, `ghcr.io/mikaelhug/kiln`, that holds both the server and the small helper that runs inside build pods.

## Requirements

- **Kubernetes 1.33 or newer**, or 1.30–1.32 with the `UserNamespacesSupport` feature gate. Older versions silently drop `hostUsers: false`, and builds then run without user namespaces.
- **User namespaces on the nodes.** Some distributions turn them off; on Talos, set the sysctl `user.max_user_namespaces` (for example `11255`) on every node.
- **A CNI that enforces NetworkPolicy**, such as Cilium, Calico, or kube-network-policies.
- **A CSI storage class with `ReadWriteOncePod`** for Kiln's database, and one with `ReadWriteOnce` for build caches.
- **A way in from the internet:** the Gateway API with a Gateway that accepts routes from the `kiln` namespace, or an Ingress controller. GitHub must reach `/hooks/github`, and your browser the UI.
- **An OCI registry**, such as GitHub Container Registry.

## 1. Create the GitHub App

```bash
kiln setup --url https://kiln.example.com
```

`kiln setup` opens a page that sends you to GitHub with the app already filled in: permissions, events, webhook and sign-in callback. GitHub shows everything before it creates the app, and you can rename it there. Add `--org <name>` to create it in an organization.

Afterwards Kiln writes `secrets.yaml`, readable only by you, with the app's private key, webhook secret and client secret. It also prints the values for `config.yaml` and the link to install the app. Install the app on **selected repositories** only.

<details>
<summary>Creating the app by hand</summary>

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
| Subscribe to events | Push · Pull request · Check run · Create · Repository |
| Where can this app be installed | Only on this account |

Then generate a private key, note the App ID and the Client ID, generate a client secret, and fill them into `deploy/kubernetes/secrets.yaml` and `config.yaml` yourself.

</details>

Webhooks are only hints. Kiln polls every repository with conditional requests, so it keeps working when a webhook is lost or the app is temporarily unreachable.

## 2. Registry credentials

Kiln needs two tokens.

**A push token.** Kiln pushes candidates and releases with it. For GitHub Container Registry, use a classic personal access token with only `write:packages`, and give it a one-year expiry. A release tag deploys straight to production, so this is the most sensitive secret Kiln holds. Only the server and the `push` step of a build ever see it. Put it in both places `secrets.yaml` asks for:
- `registry-password` of the `kiln` Secret, which the server uses to promote releases;
- `password` of the `registry-push` Secret in `kiln-builds`, which the push step uses.

Set `KILN_REGISTRY_USERNAME` and the `registry-push` username to the login that owns the token.

**A pull token.** Verify jobs start the freshly built, private candidate image, so the build namespace needs an image pull secret. Use a separate token with only `read:packages`:

```bash
kubectl -n kiln-builds create secret docker-registry registry-pull \
  --docker-server=ghcr.io --docker-username=<login> --docker-password=<read token>
```

`KILN_PULL_SECRET` names it (`registry-pull` in the example configuration).

## 3. Prepare the cluster

Builds run in the `kiln-builds` namespace, one pod per build. Each pod runs BuildKit as a sidecar.

**BuildKit mode.** Choose with `KILN_BUILDKIT_ROOTLESS`:

- `true` (default): the rootless image as uid 1000. rootlesskit needs `newuidmap`, a subuid range inside the pod's user namespace, and privilege escalation for that one container.
- `false`: the regular image as uid 0. Kiln only allows this together with `KILN_USER_NAMESPACES=true`, where that uid 0 is unprivileged on the node. On clusters with user namespaces this is often the simpler choice.

In both modes the `buildkitd` container runs with unconfined seccomp and AppArmor, which BuildKit needs to create its own sandboxes; the admission policy allows that for this container only. Pin the image by digest, and start with `KILN_BUILDKIT_ARGS=--oci-worker-no-process-sandbox`:

```bash
docker buildx imagetools inspect moby/buildkit:v0.26.0-rootless
```

Then check on your cluster that:
1. a build of a simple Dockerfile works;
2. a second build reuses the cache;
3. builds finish at your pod network MTU. Kiln runs BuildKit with `--oci-worker-net=host`, so `RUN` steps inherit the pod's MTU.

**Cache storage.** Each repository gets a `ReadWriteOnce` claim `kiln-cache-<owner>-<repo>-<hash>`, created on demand (`KILN_CACHE_SIZE`, `KILN_CACHE_STORAGE_CLASS`). A build only ever mounts its own repository's cache. Pull requests use a separate directory in it, so they cannot poison the default branch's cache. With node-local storage, a repository's builds always land on the node that holds its cache; make sure that node can run builds, or use network-attached storage.

**Network.** The example policies allow build pods to reach DNS and the internet on ports 80 and 443, and nothing else. That blocks the LAN, the cluster network and the Kubernetes API.

The server needs the Kubernetes API. Network policies see the address after DNAT, so allow both the service address and the API server's real address. Replace `10.96.0.1` and `192.0.2.10:6443` in `network-policies.yaml` with what these print:

```bash
kubectl get svc kubernetes -n default -o jsonpath='{.spec.clusterIP}'
kubectl get endpointslice -n default -l kubernetes.io/service-name=kubernetes \
  -o jsonpath='{range .items[*].endpoints[*]}{.addresses[0]}{"\n"}{end}'
```

The server may reach public HTTPS addresses only. A registry on a private address or another port needs its own egress rule.

## 4. Deploy

`deploy/kubernetes` is a kustomization with everything Kiln needs:
- the namespaces and their Pod Security levels;
- RBAC that lets Kiln manage pods, pod logs, fetch-token secrets and cache claims in `kiln-builds`. Kiln can create and delete secrets there but never read them;
- a ValidatingAdmissionPolicy that refuses build pods with a service account token, host namespaces, host paths, privileged containers or added capabilities;
- a PriorityClass and a ResourceQuota for builds;
- the network policies, the server, its storage and its route;
- `config.yaml` with the settings, and `secrets.yaml` with the credentials.

Copy it into your GitOps repository, replace `secrets.yaml` with the file `kiln setup` wrote, and adapt:

| File | What to set |
|---|---|
| `config.yaml` | every `example` value, and `KILN_IMAGE` |
| `secrets.yaml` | the registry token, in both places |
| `server.yaml` | the image, the `HTTPRoute` parent (`traefik/public`) and hostname; use an `Ingress` instead if you have no Gateway API |
| `network-policies.yaml` | the namespace of your ingress controller (`traefik`) that may reach the server, and the API server addresses |
| `build-policy.yaml` | the ResourceQuota: at least `KILN_SLOTS` times the requests of your largest build. Every test and verify job and `buildkitd` requests `KILN_DEFAULT_CPU` and `KILN_DEFAULT_MEMORY` unless a job sets its own |

**The image.** Use the same release for the server image in `server.yaml` and for `KILN_IMAGE`, pinned by the digest from the release notes, for example `ghcr.io/mikaelhug/kiln:0.1.3@sha256:…`. The server and its in-pod helper speak a private protocol, so they must always match; upgrade both together.

Encrypt `secrets.yaml` before committing it, for example with sops, and let your GitOps tool decrypt it. Without GitOps:

```bash
kubectl apply -k deploy/kubernetes
```

The server runs as a single replica on a `ReadWriteOncePod` volume, so two Kiln processes can never share the database. When it is up, sign in at your public URL with a GitHub account listed in `KILN_ALLOWED_USER_IDS`.

You can also deploy before creating the GitHub App: without `KILN_APP_ID`, Kiln serves a **Connect GitHub** page and builds nothing. Add the app's values and the secrets later; Kiln starts building after its next restart.

## 5. Onboard a repository

1. Add the repository to the GitHub App installation.
2. Commit a `.kiln.toml` with `[release] shadow = true`. Kiln runs everything except the release and reports the version it *would* release.
3. When the shadow runs look right, remove `shadow = true` and delete the old CI workflow in the same commit.

Release candidates continue above the highest `-rc.N` already in the registry, so image automation keeps working when you switch from another CI.

**Repository secrets.** Values that builds need, such as a build-time key, live in a Secret in `kiln-builds` that you create yourself; Kiln can't read it. The repository page shows its exact name under **Pipeline**: `kiln-repo-<owner>-<repo>-<hash>`, from the lowercased name with every run of other characters turned into one `-`, and a hash of `owner/repo` so two repositories never share one.

```bash
kubectl -n kiln-builds create secret generic kiln-repo-acme-web-1a2b3c4d --from-literal=ACTIONS_KEY=...
```

Reference its keys from `[[images]].secrets` or a job's `secrets`. Only trusted builds receive them: default-branch pushes, tags reachable from the default branch, and builds of the default-branch head started from the UI. Pull requests never do.

## 6. Configuration reference

Values marked *file* can also be given as `<NAME>_FILE` pointing at a mounted secret. Lists are comma-separated.

| Variable | Default | |
|---|---|---|
| `KILN_PUBLIC_URL` | required | https origin of the UI, without a path, used for links, sign-in and form checks |
| `KILN_APP_ID` | required | GitHub App ID |
| `KILN_APP_PRIVATE_KEY` | required, *file* | GitHub App private key (PEM) |
| `KILN_WEBHOOK_SECRET` | required, *file* | webhook secret |
| `KILN_WEBHOOK_SECRET_PREVIOUS` | *file* | the previous webhook secret while rotating |
| `KILN_OAUTH_CLIENT_ID` | required | GitHub App client ID |
| `KILN_OAUTH_CLIENT_SECRET` | required, *file* | GitHub App client secret |
| `KILN_ALLOWED_USER_IDS` | required | numeric GitHub user IDs allowed to sign in (`gh api users/<login> -q .id`) |
| `KILN_OWNERS` | required | accounts whose repositories Kiln builds |
| `KILN_INCLUDE_PUBLIC` | `false` | also build public repositories |
| `KILN_GITHUB_API` / `KILN_GITHUB_WEB` | `https://api.github.com` / `https://github.com` | for GitHub Enterprise Server |
| `KILN_REGISTRY` | required | registry and namespace without a scheme, e.g. `ghcr.io/example` |
| `KILN_REGISTRY_URL` | `https://<registry host>` | |
| `KILN_REGISTRY_USERNAME` | `kiln` | owner of the push token |
| `KILN_REGISTRY_PASSWORD` | required, *file* | push token |
| `KILN_IMAGE` | required | Kiln's own image for the in-pod helper, pinned by digest, same release as the server |
| `KILN_BUILDKIT_IMAGE` | required | BuildKit image, pinned by digest |
| `KILN_BUILDKIT_ARGS` | | extra `buildkitd` flags |
| `KILN_BUILDKIT_ROOTLESS` | `true` | run BuildKit as uid 1000 with the rootless image, or as uid 0 inside the pod's user namespace; `false` requires `KILN_USER_NAMESPACES=true` |
| `KILN_BUILD_NAMESPACE` | `kiln-builds` | |
| `KILN_PULL_SECRET` | | image pull secret in the build namespace |
| `KILN_PUSH_SECRET` | `kiln-registry-push` | Secret with `username` and `password` used by the push step |
| `KILN_PRIORITY_CLASS` | | PriorityClass for build pods |
| `KILN_SANDBOX_RUNTIME_CLASS` | | RuntimeClass, such as `gvisor`, for build pods that run no BuildKit: pull request builds |
| `KILN_USER_NAMESPACES` | `true` | run build pods with `hostUsers: false` |
| `KILN_AVOID_POD_LABELS` / `KILN_AVOID_POD_NAMESPACE` | | `key=value` labels and namespace of pods whose node builds avoid, e.g. the Kiln server |
| `KILN_SLOTS` | `2` | builds that run at the same time |
| `KILN_POLL_SECONDS` | `60` | how often Kiln checks each repository, at least 15 |
| `KILN_BUILD_TIMEOUT_SECONDS` | `1800` | per build, at most 2 hours |
| `KILN_MAX_ATTEMPTS` / `KILN_RETRY_SECONDS` | `3` / `30` | retries after infrastructure failures, with exponential backoff |
| `KILN_RELEASE_WINDOW_SECONDS` | `1800` | how long a failing release is retried before the build fails, at most a day |
| `KILN_DEFAULT_CPU` / `KILN_DEFAULT_MEMORY` / `KILN_MEMORY_LIMIT_FACTOR` | `1` / `2Gi` / `2` | resources of jobs that set none; the memory limit is the request times the factor |
| `KILN_WORKSPACE_SIZE` / `KILN_OUTPUT_SIZE` | `8Gi` / `10Gi` | per-build scratch space |
| `KILN_CACHE_SIZE` / `KILN_CACHE_STORAGE_CLASS` | `20Gi` / default class | per-repository cache |
| `KILN_REPO_SECRET_PREFIX` | `kiln-repo-` | |
| `KILN_DATA_DIR` | `/data` | database, logs and the hourly backup |
| `KILN_LISTEN` | `0.0.0.0:8080` | |

## 7. Runbook

| Situation | What to do |
|---|---|
| A build fails with a strange cache error | **Reset build cache** in the repository's menu. The next build starts cold. |
| Kiln restarted during a build | Nothing. Kiln re-attaches to running pods, re-reads their logs and resumes releases. |
| A node was drained during a build | Kiln retries the build on another node, up to `KILN_MAX_ATTEMPTS`. |
| Builds fail to start with "exceeded quota" | Raise the ResourceQuota in `build-policy.yaml`, or lower `KILN_SLOTS` or job resources. |
| GitHub is down | Builds wait. Kiln catches up from polling, and check runs are retried from an outbox. |
| The database is lost | Delete the claim and restart. Version numbers continue from the registry; only history is lost. |
| A release failed with "semver tags are immutable" | Someone or something already published that version with a different image. Release a new version. |
| Rotate the webhook secret | Set the new value and keep the old one in `KILN_WEBHOOK_SECRET_PREVIOUS` until GitHub uses the new one. |
| Rotate the App key or the client secret | GitHub allows two at a time: add the new one, deploy, then delete the old one. |
| Upgrade Kiln | Change the server image and `KILN_IMAGE` to the new release together, ideally while no build runs. |
| Stop all builds for a repository | **Pause builds** in the repository's menu. |
| A repository shows "Published outside Kiln" | Every hour Kiln compares the registry's semver tags with what it released. Another CI still publishing, or someone using the registry credential, shows up here. Find the source, then **Dismiss**. |

Kiln writes a copy of its database to `kiln.backup.db` in the data directory every hour.

## 8. Try it without a cluster

```bash
docker run --rm -p 8080:8080 ghcr.io/mikaelhug/kiln:0.1.3 demo --bind 0.0.0.0
```

The demo runs the real engine and UI against simulated repositories, builds and logs. It starts with a few days of build history and pushes a new commit every minute or so. Nothing outside your machine is contacted.
