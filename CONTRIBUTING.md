# Contributing

Thanks for helping. Kiln is small on purpose, so please open an issue to discuss a feature before writing it.

## Getting started

You need Rust (the version in `rust-toolchain.toml` installs itself), [just](https://github.com/casey/just), Python 3 and Docker.

```bash
just check
cargo run -p kiln -- demo
```

`just check` runs formatting, clippy, the no-comments rule, the tests and the schema drift check. The registry tests start a `registry:3` container. `kiln demo` runs the real engine and UI against simulated repositories on <http://localhost:8080>.

## House rules

- **No code comments.** Names and small functions carry the meaning; `scripts/no_comments.py` enforces it.
- **Few, purposeful tests.** A test pins a rule: versioning, scheduling and supersede, the secret trust gate, redaction, config validation and path containment, pod-spec security invariants, promotion, or a regression for a real bug.
- **Reuse before writing.** Search the workspace before adding a helper, type or dependency.
- **Minimal surface.** Delete dead code, unused exports and dependencies in the same change that makes them unused.
- **Docs move with code.** Update `docs/` and `schema/kiln.v1.json` (`just schema`) in the same change.
- **One event stream per page.** The web UI pushes updates over server-sent events and never polls.
- **Security first.** The registry credential never reaches BuildKit, test containers or pull-request builds, and pull requests never receive secrets.

## Pull requests

Keep a pull request to one change, describe what it does and how you checked it, and add a line to `CHANGELOG.md` under **Unreleased** when users would notice it.
