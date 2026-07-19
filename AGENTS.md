# AGENTS.md

## Project scope

This repository contains the FJX High-Performance Ledger Service technical assessment.

Implement only work explicitly requested by the current GitHub issue or user instruction. Do not broaden the scope, add speculative features, or redesign agreed architecture without first reporting the conflict.

## Sources of truth

Use the following precedence:

1. `README.md` — official assessment requirements;
2. the current GitHub issue or explicit user instruction;
3. `design.md` — approved design and implementation direction;
4. existing tests and code;
5. this file.

Do not modify `README.md`. It is provided by the assessment owner and represents the original task requirements.

For tasks that do not explicitly concern architecture or design, treat `design.md` as authoritative. Follow its decisions and constraints rather than introducing alternative designs.

For a task explicitly intended to change the design, update `design.md` first or as part of the same focused change. Do not silently make the implementation diverge from the documented design.

If sources conflict, report the conflict before making a design-changing implementation.

## License

- Read and comply with the repository `LICENSE` file.
- Treat `LICENSE` as externally provided, out of scope for the assessment, and immutable.
- Do not edit, replace, rename, reformat, regenerate, or delete `LICENSE`.
- Preserve all copyright and permission notices required by the license in copies or substantial portions of the software.
- If a requested change appears to conflict with the license, stop and report the conflict before proceeding.

## Development workflow

- Work from a dedicated branch created from the latest `origin/main`.
- Keep each issue and pull request focused on one logical change.
- Do not commit directly to `main`.
- Do not push working branches to the `target` remote.
- `origin` is the private development repository.
- `target` is used only for final delivery of verified `main`.
- Prefer small, reviewable commits while working; pull requests may be squash-merged.
- Do not rewrite shared branch history unless explicitly requested.

## Implementation rules

- Preserve correctness, atomicity, idempotency, and data consistency.
- Prefer simple, explicit solutions over speculative abstractions.
- Do not introduce new services, infrastructure, dependencies, caches, or background processing unless required by the issue or approved design.
- Keep domain logic separate from transport and persistence concerns where practical.
- Handle errors explicitly; do not silently ignore failures.
- Never log secrets, credentials, access tokens, or sensitive user data.
- Do not change unrelated files.
- Do not modify requirements or tests merely to match an implementation.

## Rust quality

Before marking implementation work complete, run the applicable checks:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

When relevant, also verify the application, database migrations, and infrastructure through the documented local workflow.

Do not claim that a check passed unless it was actually executed. Report commands that could not be run and why.

## Tests

- Add or update tests for changed behavior.
- Cover important failure paths and boundary conditions.
- Prefer deterministic tests.
- Do not remove or weaken tests solely to make a change pass.

## Documentation

Update documentation when behavior, configuration, API contracts, setup, or operational procedures change.

Keep documentation concise and aligned with the implemented state. Do not document hypothetical components as if they already exist.

## Pull request completion

A pull request should state:

- what changed;
- why it changed;
- how it was verified;
- any remaining risks or limitations.

Before completion, review the final diff for unrelated changes, generated files, secrets, accidental formatting churn, and any change to `README.md` or `LICENSE`.