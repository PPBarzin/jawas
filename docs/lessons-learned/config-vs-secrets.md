# Lessons Learned: Configuration vs Secrets

Date: 2026-05-13

## Why this matters

Mixing strategy settings, infrastructure flags, and sensitive credentials in one place creates:

- accidental leaks
- poor auditability
- harder operations and rollback
- unclear source of truth

## Core rule

Treat **configuration** and **secrets** as different classes of data.

- Configuration: safe to read by developers and operators.
- Secrets: must be protected, rotated, and never committed.

## Practical split

### 1. Config file (for non-sensitive settings)

Use a versioned file (e.g. `config.yaml`) for:

- strategy thresholds
- timing windows
- feature modes
- non-sensitive runtime defaults

Benefits:

- readable diffs
- reviewable changes
- easier future UI integration

### 2. Secret store (for credentials)

Use Docker/K8s/secret manager mounted files for:

- API keys
- private endpoints with auth
- signing credentials
- tokens/passwords

Benefits:

- less exposure than env vars
- cleaner rotation
- better production hygiene

### 3. Environment variables (for deployment wiring and controlled overrides)

Use env vars for:

- deployment-specific endpoints
- operational toggles
- temporary emergency overrides

Avoid putting long-term business settings only in env vars.

## Runtime pattern

Load once and merge in deterministic order:

1. code defaults
2. `config.yaml`
3. env overrides
4. secrets from secret files

Then log a startup summary with each key origin (`default|file|env|secret`) without printing secret values.

## Docker guidance

- Do not bake secrets into images.
- Prefer mounted secrets (`/run/secrets/...`) or orchestrator secret managers.
- Keep config files in mounted volumes if live updates are needed.
- Rebuild only when code changes, not for every config change.

## Anti-patterns

- `.env` used as a single bucket for everything.
- secrets committed in git or copied in plaintext docs.
- hot-path file reads for frequently accessed settings.
- no visibility on which source produced the effective runtime value.

## Decision heuristic

Ask for every variable:

- If leaked, does it create security/financial/legal risk?
  - Yes -> secret store.
  - No -> config/env depending on operational need.

## Bottom line

`config.yaml` improves product control and clarity.  
Secret files or secret managers improve security posture.  
Env vars remain useful, but should not be the only configuration system.
