# Lessons Learned: Multi-Agent Roles and Model Assignment

Date: 2026-05-13

## Context

A single agent handling everything (Airtable operations, technical writing, coding, planning/execution) reaches practical limits quickly:

- context saturation
- token budget pressure
- reduced quality on long threads
- slower execution due to role-switching overhead

For Jawas, work should be split into specialized sub-agents with explicit responsibilities and model assignment.

## Agent 1: Airtable Operator

Purpose:

- retrieve records from Airtable
- create/update tasks and structured fields (`Task`, `Description`, `Resolution`, `X`, etc.)
- keep backlog consistency with project context and recent findings

Inputs:

- current incident/report context
- target Airtable base/table/record scope
- task governance rules (`AGENTS.md`)

Outputs:

- updated Airtable records
- concise change summary (what was changed and why)

Recommended model:

- `gpt-5.4-mini` for routine CRUD updates
- escalate to `gpt-5.4` when field rewriting quality is critical

## Agent 2: Documentation Curator

Purpose:

- update project documentation (`README`, specs, analyses, lessons learned)
- keep positioning accurate: experimental, technical, non-marketing
- ensure docs reflect as-built behavior

Inputs:

- implemented code behavior
- change logs and runtime observations
- documentation constraints from `AGENTS.md`

Outputs:

- updated docs aligned with current runtime
- explicit deltas between old and new behavior

Recommended model:

- `gpt-5.4` for stable high-quality technical writing

## Agent 3: Implementation Engineer

Purpose:

- implement code changes
- run checks/tests
- deliver reliable, reviewable diffs

Inputs:

- decision-complete implementation spec
- runtime constraints and env assumptions

Outputs:

- code changes
- test/check results
- technical risk notes

Recommended model:

- `gpt-5.5` for complex cross-module implementation
- `gpt-5.3-codex` for focused coding-heavy patches

## Agent 4: Supervisor (Planner + Executor)

Purpose:

- define execution plan
- sequence/parallelize work across agents
- validate completion criteria
- consolidate final result

Inputs:

- user objective and constraints
- repo and runtime state
- outputs from specialist agents

Outputs:

- execution plan
- orchestration decisions
- final integrated delivery

Recommended model:

- `gpt-5.5` (strongest reasoning + integration role)

## Operating Rule

Default split for non-trivial work:

1. Supervisor frames and decomposes.
2. Implementation Engineer codes.
3. Documentation Curator updates docs.
4. Airtable Operator updates backlog records.
5. Supervisor validates and closes.

This reduces context overload and preserves output quality over long sessions.
