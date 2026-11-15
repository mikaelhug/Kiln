# Operating Kiln

This guide takes you from nothing to a first build. Kiln ships as one image, `ghcr.io/mikaelhug/kiln`, that holds both the server and the small helper that runs inside build pods.

## Requirements

- **Kubernetes 1.30 or newer.**
- **User namespaces on the nodes**, which rootless BuildKit uses to sandbox image builds. Some distributions turn them off; on Talos, set the sysctl `user.max_user_namespaces` (for example `11255`) on every node.
- **A CNI that enforces NetworkPolicy**, such as Cilium, Calico, or kube-network-policies.
- **A CSI storage class with `ReadWriteOncePod`** for Kiln's database, and one with `ReadWriteOnce` for build caches.
- **A way in from the internet:** the Gateway API with a Gateway that accepts routes from the `kiln` namespace, or an Ingress controller. GitHub must reach `/hooks/github`, and your browser the UI.
- **An OCI registry**, such as GitHub Container Registry.
- **Optional: gVisor** (a `gvisor` RuntimeClass) to sandbox pull request builds.

## 1. Deploy

`deploy/kubernetes` is a kustomization with everything Kiln needs:
- the namespaces and their Pod Security levels;
- RBAC that lets Kiln manage pods, pod logs, secrets and cache claims in `kiln-builds`. Kiln can create and delete secrets there but never read them;
- a ValidatingAdmissionPolicy that refuses build pods with a service account token, host namespaces, host paths, privileged containers or added capabilities;
- a PriorityClass and a ResourceQuota for builds;
- the network policies, the server, its storage and its route;
- `config.yaml` with the settings.

There are no secrets to prepare: Kiln creates its GitHub App and keeps its credentials itself. Copy the kit into your GitOps repository and adapt:

| File | What to set |
|---|---|
| `config.yaml` | `KILN_PUBLIC_URL` and `KILN_IMAGE` |
| `server.yaml` | the image, the `HTTPRoute` parent (`traefik/public`) and hostname; use an `Ingress` instead if you have no Gateway API |
| `network-policies.yaml` | the namespace of your ingress controller (`traefik`) that may reach the server, and the API server addresses |
| `build-policy.yaml` | the ResourceQuota: at least `KILN_SLOTS` times the requests of your largest build |

**The image.** Use the same release for the server image in `server.yaml` and for `KILN_IMAGE`, pinned by the digest from the release notes, for example `ghcr.io/mikaelhug/kiln:0.2.1@sha256:…`. The server and its in-pod helper speak a private protocol, so they must always match; upgrade both together. Every push to Kiln's `main` also publishes `X.Y.(Z+1)-rc.N`; image automation with a policy such as `>=0.0.0-0` and digest reflection can set both to the latest build.

**The API server addresses.** Network policies see the address after DNAT, so allow both the service address and the API server's real address. Replace `10.96.0.1` and `192.0.2.10:6443` in `network-policies.yaml` with what these print:

```bash
kubectl get svc kubernetes -n default -o jsonpath='{.spec.clusterIP}'
kubectl get endpointslice -n default -l kubernetes.io/service-name=kubernetes \
  -o jsonpath='{range .items[*].endpoints[*]}{.addresses[0]}{"\n"}{end}'
```

Without GitOps:

```bash
kubectl apply -k deploy/kubernetes
```

## 2. Connect GitHub

Open your Kiln address. A fresh install shows **Connect GitHub** and asks for a setup code, which Kiln prints in its log so nobody else can claim it:

```bash
kubectl -n kiln logs deploy/kiln | grep code
```

Enter it, optionally with an organization, and Kiln sends you to GitHub with its app filled in: permissions, events, webhook and sign-in. Click **Create GitHub App**. Back in Kiln, choose the repositories it may build and sign in. For an app on your personal account, you are the first person who can sign in. For an organization's app, the first person is whoever signs in from the browser that connected it; if that is lost, the first person to sign in claims Kiln with a new setup code from the log.

## 3. Add a registry token

Image builds need a token that can publish to your registry. For GitHub Container Registry, create a classic token with only `write:packages` and a one-year expiry. Open **Settings › Registry**, enter the token and its owner's login, and save. Kiln restarts to use it, and creates the push and pull secrets for build pods itself.

A release tag deploys straight to production, so this token is the most sensitive secret Kiln holds. Only the server and the `push` step of a build ever see it. Until a token is saved, pull requests are tested but default-branch builds stop with a note asking for one.

## 4. Onboard a repository

1. Add the repository to the GitHub App installation (**Settings › GitHub › Choose repositories**).
2. Commit a `.kiln.toml` with `[release] shadow = true`. Kiln runs everything except the release and reports the version it *would* release.
3. When the shadow runs look right, remove `shadow = true` and delete the old CI workflow in the same commit.

Release candidates continue above the highest `-rc.N` already in the registry, so image automation keeps working when you switch from another CI.

**Repository secrets.** Values that builds need, such as a build-time key, live in a Secret in `kiln-builds` that you create yourself; Kiln can't read it. The repository page shows its exact name under **Pipeline**.

```bash
kubectl -n kiln-builds create secret generic kiln-repo-acme-web-1a2b3c4d --from-literal=ACTIONS_KEY=...
```

Reference its keys from `[[images]].secrets` or a job's `secrets`. Only trusted builds receive them: default-branch pushes, tags reachable from the default branch, and builds of the default-branch head started from the UI. Pull requests never do.

**People.** **Settings › People** lists who can sign in. Add collaborators by their GitHub username; removing someone ends their sessions.

## 5. How builds are isolated

Every build is one pod in `kiln-builds`. Image builds use a rootless BuildKit sidecar, which sandboxes `RUN` steps in its own user namespace. Its container runs with unconfined seccomp and AppArmor, which it needs to create that sandbox; the admission policy allows that for this container only.

With `KILN_SANDBOX_RUNTIME_CLASS=gvisor`, pull request builds, which run only tests and never BuildKit, run entirely under gVisor.

Build pods may reach DNS and the internet on ports 80 and 443, and nothing else, which blocks the LAN, the cluster network and the Kubernetes API. The server may reach public HTTPS addresses only; a registry on a private address or another port needs its own egress rule.

Each repository gets a `ReadWriteOnce` cache claim, created on demand. A build only mounts its own repository's cache, and pull requests use a separate directory in it, so they cannot poison the default branch's cache.

## 6. Configuration reference

Only `KILN_PUBLIC_URL` and `KILN_IMAGE` are required. Lists are comma-separated.

| Variable | Default | |
|---|---|---|
| `KILN_PUBLIC_URL` | required | https origin of the UI, without a path |
| `KILN_IMAGE` | required | Kiln's own image for the in-pod helper, pinned by digest, same release as the server |
| `KILN_REGISTRY` | `ghcr.io/<GitHub account>` | registry and namespace images are published to |
| `KILN_REGISTRY_URL` | `https://<registry host>` | |
| `KILN_INCLUDE_PUBLIC` | `false` | also build public repositories |
| `KILN_GITHUB_API` / `KILN_GITHUB_WEB` | `https://api.github.com` / `https://github.com` | for GitHub Enterprise Server |
| `KILN_BUILDKIT_IMAGE` | a pinned `moby/buildkit` rootless release | |
| `KILN_BUILDKIT_ARGS` | `--oci-worker-no-process-sandbox` | `buildkitd` flags |
| `KILN_BUILD_NAMESPACE` | `kiln-builds` | |
| `KILN_PRIORITY_CLASS` | | PriorityClass for build pods |
| `KILN_SANDBOX_RUNTIME_CLASS` | | RuntimeClass, such as `gvisor`, for build pods that run no BuildKit: pull request builds |
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
| The registry token expires | Save a new one in **Settings › Registry**. |
| The GitHub App was deleted or its key revoked | **Settings › GitHub › Disconnect**, then connect again. People and history stay. If nobody can sign in, delete Kiln's data volume instead. |
| The database is lost | Kiln shows **Connect GitHub** again; connect a new app and delete the old one on GitHub. Version numbers continue from the registry; only history is lost. |
| A release failed with "semver tags are immutable" | Someone or something already published that version with a different image. Release a new version. |
| Upgrade Kiln | Change the server image and `KILN_IMAGE` to the new release together, ideally while no build runs. |
| Stop all builds for a repository | **Pause builds** in the repository's menu. |
| A repository shows "Published outside Kiln" | Every hour Kiln compares the registry's semver tags with what it released. Another CI still publishing, or someone using the registry credential, shows up here. Find the source, then **Dismiss**. |

Kiln keeps its database, including the GitHub App's key and the registry token, on its own volume, and writes a copy to `kiln.backup.db` there every hour. Treat that volume as a secret.

## 8. Try it without a cluster

```bash
docker run --rm -p 8080:8080 ghcr.io/mikaelhug/kiln:0.2.1 demo --bind 0.0.0.0
```

The demo runs the real engine and UI against simulated repositories, builds and logs. It starts with a few days of build history and pushes a new commit every minute or so. Nothing outside your machine is contacted.
