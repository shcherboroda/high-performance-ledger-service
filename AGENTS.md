# AGENTS.md

## Project scope

This repository contains the FJX High-Performance Ledger Service technical assessment.

Implement only work explicitly requested by the current GitHub issue or user instruction. Do not broaden the scope, add speculative features, or redesign agreed architecture without first reporting the conflict.

## Sources of truth

Use the following precedence:

1. `README.md` — official assessment requirements;
2. the current GitHub issue or explicit user instruction;
3. `design.md` — approved design and implementation direction;
4. existing tests, workflows, configuration, infrastructure, and code;
5. this file.

Do not modify `README.md`. It is provided by the assessment owner and represents the original task requirements.

Do not modify, rename, replace, reformat, or delete `LICENSE`. It is outside the implementation scope.

For tasks that do not explicitly concern architecture or design, treat `design.md` as authoritative. Follow its decisions and constraints rather than introducing alternative designs.

For a task explicitly intended to change the design, update `design.md` first or as part of the same focused change. Do not silently make the implementation diverge from the documented design.

If sources conflict, report the conflict before making a design-changing implementation.

## Context loading

- Treat this file as persistent repository guidance; do not repeatedly reopen it during the same task unless it changed or a rule needs exact verification.
- Do not read `LICENSE` during normal implementation work. Its only project-specific rule is that the file must remain unchanged.
- Do not reread the complete `README.md` for every task. Consult only the relevant requirement section when the issue does not already provide sufficient task context, when requirement compliance is uncertain, or during final delivery verification.
- Do not reread the complete `design.md` for every task. Inspect only the sections relevant to the component or decision being changed. Read it more broadly only for architecture work, cross-cutting changes, or when the issue may conflict with the approved design.
- Start from the current issue and the files directly involved in the requested change. Expand repository inspection only as needed to understand dependencies, existing behavior, tests, or risks.
- Reuse information already established in the current task or review instead of fetching the same unchanged content again.
- Never skip necessary verification merely to save tokens; optimize by reading narrowly, not by guessing.

## Preserve the provided repository baseline

- Treat files already present in the original assessment repository as part of the assignment unless explicitly identified otherwise.
- Do not remove, replace, simplify, disable, or weaken existing tests, checks, workflows, configuration, infrastructure, dependencies, or safeguards merely because they are not yet used by the current implementation.
- Prefer extending the existing setup over replacing it.
- Modify an existing baseline file only when the current issue explicitly requires it or when a concrete defect blocks the requested work.
- Before changing a baseline file, inspect its purpose and preserve its existing behavior unless the approved change explicitly says otherwise.
- Report potentially obsolete, premature, or failing baseline configuration instead of deleting or reducing it without approval.

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

Before completion, review the final diff for unrelated changes, generated files, secrets, accidental formatting churn, changes to `README.md` or `LICENSE`, and any unintended weakening of the original repository baseline.
