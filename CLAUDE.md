# Kiln agent rules

Kiln is a public repository. Never commit private hostnames, IP addresses, repository names, cluster details or secrets; deployment configuration lives in the operator's private GitOps repository.

1. **No code comments** in Rust, TOML, YAML, Python or justfiles. `just lint` enforces it.
2. **Few, purposeful tests**: a test pins a rule (versioning, scheduling and supersede, the secret trust gate, redaction, config validation and path containment, pod-spec security invariants, promotion) or a regression for a real bug.
3. **Reuse before writing**: search the workspace before adding a helper, type or dependency.
4. **Minimal surface**: no speculative code; delete dead code, unused exports and dependencies in the same change that makes them unused.
5. **Docs move with code**: update `docs/` and `schema/kiln.v1.json` (`just schema`) in the same change.
6. **Push, never poll** in the web UI: one server-sent event stream per page.
7. **Security first**: the registry push credential never reaches BuildKit, test containers or pull-request builds; pull requests never receive secrets.

Run `just check` before every commit.
