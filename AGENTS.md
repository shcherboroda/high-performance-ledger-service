# Repository guidance

Keep changes focused, reviewable, and aligned with the documented design. Do not change the service's correctness, atomicity, idempotency, or consistency guarantees without updating the implementation, tests, and relevant design documentation together.

## Sources of truth

Use this order when requirements conflict:

1. explicit user or issue requirements;
2. `design.md` for architecture and trade-offs;
3. existing tests, workflows, configuration, and code;
4. this file.

## Working practices

- Work on a dedicated branch; do not commit directly to `main`.
- Keep the `target` remote untouched; it is not a development destination.
- Preserve tests, validation, migrations, CI, Docker configuration, and reproducibility tooling unless an approved change requires an update.
- Do not introduce new infrastructure, dependencies, or background processing without a concrete requirement.
- Never log or commit credentials, tokens, keys, full database URLs with credentials, or sensitive user data.
- Keep generated benchmark artifacts, local configuration, build outputs, and editor state untracked.
- Update documentation when behavior, configuration, API contracts, or operations change.

## Verification

Run the applicable checks before completion:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
python3 -m unittest discover -s benchmarks/tests
cargo run --bin generate-openapi -- --check
```

Use the documented local workflow when database, Docker, or benchmark validation is relevant. Report checks that could not be run and why.
