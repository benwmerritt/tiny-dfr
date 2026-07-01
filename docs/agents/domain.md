# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Layout for this repo

This is a single-context repo.

- Read `CONTEXT.md` at the repo root for project vocabulary.
- Read ADRs under `docs/adr/` when the area being changed touches a recorded decision.
- If an expected domain file does not exist yet, proceed silently. The domain-modeling workflow creates missing domain docs lazily when terms or decisions actually get resolved.

## Before exploring, read these

- `CONTEXT.md` at the repo root.
- `docs/adr/` for relevant architectural decisions.

## File structure

```text
/
├── CONTEXT.md
├── docs/
│   ├── agents/
│   │   ├── domain.md
│   │   ├── issue-tracker.md
│   │   └── triage-labels.md
│   └── adr/
└── src/
```

## Use the glossary's vocabulary

When your output names a domain concept in an issue title, refactor proposal, hypothesis, test name, or docs change, use the term as defined in `CONTEXT.md`. Do not drift to synonyms the glossary explicitly avoids.

If the concept you need is not in the glossary yet, either reconsider whether it belongs to this project's domain language or note the gap for the domain-modeling workflow.

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> Contradicts ADR-0001 — worth reopening because...
