# AGENTS.md / CLAUDE.md

This repository is Ben's experimental fork of upstream `AsahiLinux/tiny-dfr`.
Agents are explicitly authorised to inspect, plan, edit, test, and commit work in this fork when Ben asks.

The upstream no-agent notice previously copied into this fork is intentionally replaced here and does not apply to Ben's fork.

## Project goal

Build a safer local tiny-dfr fork for Ben's Arch/T2 MacBook Touch Bar that can support:

- workspace buttons on the left;
- breathing room between workspace buttons and controls;
- no date, battery, time, or screenshot buttons in Ben's preferred layout;
- expandable control groups for brightness, keyboard/Touch Bar backlight, volume, and media;
- active Niri workspace highlighting through a narrow state interface;
- a rollback path to stock tiny-dfr if the Touch Bar glitches.

## Agent skills

### Issue tracker

Issues and PRDs are tracked as local markdown under `.scratch/<feature-slug>/`; external PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the default five triage roles: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

This is a single-context repo: use root `CONTEXT.md` for vocabulary and root `docs/adr/` for architectural decisions when present. See `docs/agents/domain.md`.

## Safety rules

- Do not live-install, replace, restart, or stop the running Touch Bar service unless Ben explicitly approves that action in the current chat.
- Do not use `sudo` for install/service/hardware operations unless Ben explicitly approves.
- Keep stock `tiny-dfr` recoverable. Experimental installs should use a distinct name such as `tiny-dfr-ben` and a separate service.
- Do not force-switch the `05ac:8302` USB configuration. That can wedge the Touch Bar display until reboot on this machine.
- Do not relax `ProtectHome=true` as a shortcut for Niri integration.
- Do not add arbitrary shell-command execution inside tiny-dfr.
- Do not use runtime config rewrites as the long-term state/control plane for workspace highlighting.

## Normal workflow

1. Check `git status --short --branch` before editing.
2. Preserve user changes; stage only files touched for the current task.
3. Prefer small, reviewable commits.
4. Keep one writer in the active worktree at a time. Use subagents for read-only scouting/review unless explicitly doing a single-writer handoff.
5. Validate Rust changes with the strongest available checks:

   ```sh
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   cargo build
   ```

   If the Rust toolchain is unavailable, record that clearly and do not pretend validation passed.

## Planning references

- Approved plan: `/home/ben/plans/md/2026-07-01-tiny-dfr-niri-touchbar-fork-plan.md`
- Research project: `/home/ben/md-vaults/agent-vault/research/tiny-dfr-niri-touchbar/`
- Ben's Arch config repo: `/home/ben/archdots`

## Remotes

Use Ben's fork as `origin` and upstream as `upstream`:

```sh
git remote add upstream git@github.com:AsahiLinux/tiny-dfr.git
```

Fetch upstream regularly, but do not push to upstream.
