# `.kiln.toml`

A repository opts in by committing `.kiln.toml` at its root. Kiln reads it from the commit being built. Unknown keys are errors. Editors that use Taplo get completion from the schema header.

```toml
#:schema https://raw.githubusercontent.com/mikaelhug/kiln/main/schema/kiln.v1.json
version = 1

[on]
push = true
tags = true
pull_request = true

[release]
mode = "rc"
shadow = false

[[images]]
name = "app"
file = "Dockerfile"
context = "."
args = { DEPLOYMENT_ID = "${sha}" }
secrets = { actions_key = "NEXT_SERVER_ACTIONS_ENCRYPTION_KEY" }

[test.unit]
image = "node:26"
cache = ["/root/.npm"]
steps = ["npm ci", "npm test"]
```

`version = 1` on its own builds the image named after the repository from `Dockerfile`.

## Phases

Every run goes through the same phases. Pull requests stop after the tests.

1. **test**: each `[test.<job>]` runs its steps in order in its `image`, sharing the workspace.
2. **build**: each `[[images]]` entry is built with BuildKit and pushed as a candidate tagged `sha-<commit>`.
3. **verify**: each `[verify.<job>]` runs next to a `candidate` service started from a freshly built image.
4. **release**: the candidates are tagged with the version, in the order of `[[images]]`.

A failing test or verify job stops the run before anything is released.

## `[on]`

| Key | Default | Runs on |
|---|---|---|
| `push` | `true` | pushes to the default branch |
| `tags` | `true` | tags `vX.Y.Z` or `vX.Y.Z-pre` whose commit is on the default branch; other tags are never built |
| `pull_request` | `true` | pull request heads: tests only, without secrets |

A head commit message containing `[skip ci]` or `[skip kiln]` is not built.

## `[release]`

- `mode = "rc"`: a push to the default branch releases `X.Y.(Z+1)-rc.N`. `X.Y.Z` is the highest `vX.Y.Z` tag, and `N` continues above every rc already in the registry. A tag `vA.B.C` releases `A.B.C`, reusing the candidate when that commit was already built and verified.
- `mode = "tags"`: only tags release; default-branch pushes build and verify a candidate.
- `shadow = true`: everything runs except the release. Use it while migrating from another CI.

A release fails if its version would not be higher than the highest semver tag already in the registry, because image automation would never deploy it. Semver tags are immutable.

## `[[images]]`

| Key | Default | |
|---|---|---|
| `name` | | registry repository name |
| `file` | `Dockerfile` | relative to the repository root |
| `context` | `.` | relative to the repository root |
| `target` | last stage | Dockerfile stage |
| `platforms` | `["linux/amd64"]` | |
| `args` | | build arguments |
| `secrets` | | maps a BuildKit secret id to a key in the repository's secret |
| `labels` | | added to Kiln's `org.opencontainers.image.*` labels |
| `resources` | | `{ cpu = "4", memory = "8Gi" }` for BuildKit while it builds; the largest value across images wins |

`args` and `labels` may use `${sha}`, `${short_sha}`, `${ref}`, `${version}` and `${run}`. Write `$${` for a literal `${`.

## Jobs: `[test.<name>]` and `[verify.<name>]`

| Key | |
|---|---|
| `image` | container image the steps run in |
| `steps` | commands, or `{ name, run }` tables, run with `bash -eo pipefail` (or `sh -e`) |
| `setup` | steps run before services start |
| `env` | literal environment variables |
| `secrets` | keys of the repository's secret exposed as environment variables, on trusted runs only |
| `cache` | directories kept between runs of the repository |
| `services` | sidecars reachable on `localhost`; they start before the job and keep running until the build ends |
| `timeout` | `"20m"` style, at most `"2h"` |
| `resources` | `{ cpu = "1", memory = "2Gi" }` |

Each step runs in its own process group. When a step ends, anything it started in the background is stopped, so a server needed by later steps belongs in `services`.

Steps see `CI=true`, `KILN_REPOSITORY`, `KILN_SHA`, `KILN_REF`, `KILN_EVENT` and `KILN_RUN`, and `KILN_VERSION` except on pull requests. In verify jobs they also see `KILN_IMAGE_<NAME>` with the candidate's image reference; `<NAME>` is the image name in upper case with `-` and `.` turned into `_`.

### Services

A service sets exactly one of `image` or `candidate`. `candidate` names one of the `[[images]]` and is only allowed in verify jobs.

| Key | |
|---|---|
| `name` | |
| `image` / `candidate` | |
| `env`, `secrets` | as for jobs |
| `user` | numeric user and group, default `65532`; services run as non-root without capabilities, so they listen on ports above 1024 |
| `read_only` | read-only root filesystem |
| `tmpfs` | memory-backed writable directories |
| `mounts` | `{ "/container/path" = "dir/in/repo" }`, a directory of the repository mounted read-only |
| `expect_exit` | exit code required when Kiln stops the service at the end of the build |
| `port` | the job starts only once the service accepts connections on this port, such as `5432` for Postgres |

## Validate locally

```bash
kiln validate
kiln plan --event push --published 0.0.1-rc.55
kiln plan --event tag:v1.0.0
kiln plan --event pr
```
