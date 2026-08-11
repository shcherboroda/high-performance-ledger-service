# Repository guidance

Keep changes focused, reviewable, and aligned with the documented design. Do not change the service's correctness, atomicity, idempotency, or consistency guarantees without updating the implementation, tests, and relevant documentation together.

## Scope and sources of truth

Implement only the work explicitly requested by the user or the current GitHub issue. Do not broaden scope, redesign agreed architecture, or add speculative features without first explaining the conflict.

When sources conflict, use this order:

1. explicit user or issue requirements;
2. `design.md` for architecture, invariants, and trade-offs;
3. relevant README and operational documentation for supported behavior and workflows;
4. existing tests, workflows, configuration, and code;
5. this file.

If an implementation would diverge from the documented design, update the relevant design documentation as part of the same focused change or report the conflict first.

## Context loading

- Start with the current task and the files it directly affects; expand inspection only as needed to understand dependencies, behavior, and risk.
- Read documentation narrowly and reuse already established context. Do not skip necessary verification to save time.
- Treat files present in the repository as part of the maintained baseline unless the task explicitly identifies them as obsolete.

## Change integrity

- Preserve tests, validation, migrations, CI, Docker configuration, and reproducibility tooling unless an approved change requires an update.
- Do not remove, disable, weaken, or rewrite requirements, checks, or tests merely to make a change pass.
- Do not change unrelated files. Prefer small, explicit solutions over speculative abstractions.
- Keep domain logic separate from transport and persistence concerns where practical, and handle errors explicitly.
- Add or update deterministic tests for changed behavior and important failure paths.
- Update documentation when behavior, configuration, API contracts, setup, or operations change. Do not document hypothetical behavior as implemented.

## Security and local state

- Never log or commit credentials, access tokens, private keys, full database URLs with credentials, or sensitive user data.
- Keep generated benchmark artifacts, local configuration, build outputs, and editor state untracked.
- Use the documented local workflow for database, Docker, and benchmark work. Run destructive benchmark setup only against a dedicated database whose name ends in `_benchmark`, with its required acknowledgement.

## Git and pull requests

- Work on a dedicated branch created from the current `origin/main`; do not commit directly to `main`.
- Keep each pull request focused on one logical change. Prefer small, reviewable commits.
- Do not push to the `target` remote unless explicitly instructed.
- Do not rewrite shared history unless explicitly requested.
- Before delivery, review the final diff for unrelated changes, generated artifacts, secrets, accidental formatting churn, and weakened safeguards.
- A pull request summary must state what changed, why, how it was verified, and any remaining risks or limitations.

## Verification

Run the applicable checks before completion:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
python3 -m unittest discover -s benchmarks/tests
cargo run --bin generate-openapi -- --check
```

Database-backed tests require PostgreSQL; use `./scripts/test-local.sh` when integration validation applies. Report every check that could not be run and why.
