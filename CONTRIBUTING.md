# Contributing

Jawas is a research codebase. Contributions should improve clarity, instrumentation, or correctness before they try to expand features.

## Expected Contribution Style

- preserve behavior unless a bug fix is explicitly intended
- prefer small, reviewable refactors
- keep protocol-specific logic readable
- add tests for pure logic when extracting or modifying it
- document assumptions instead of hiding them

## Pull Request Checklist

- `cargo check`
- `cargo test`
- update docs if runtime behavior or configuration changed
- avoid committing secrets, private endpoints, or personal wallet state

## Scope Guardrails

- do not present speculative code as production-ready
- do not add frameworks unless they remove clear complexity
- do not bury research limitations behind marketing language
