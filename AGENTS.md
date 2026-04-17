# AGENTS.md

This file captures stable repo-local working rules for WaveRunner.

Keep it short, practical, and limited to rules that should survive across sessions.

## Repo Identity

- WaveRunner is an RTL-SDR-first Rust SDR workbench with CLI, TUI, and GUI frontends.
- Protect operator trust. Docs, UI copy, and release notes must state what is verified, what depends on external tools, and what still falls short.

## Working Defaults

- Be direct, blunt, and high-signal. No fluff.
- Prefer doing the work over proposing the work when the change is local, reversible, and verifiable.
- Do not commit, tag, or push unless explicitly asked. Before any commit/publish step, make sure the changelog and docs reflect reality.
- Keep code concrete. Avoid fake abstraction, dead genericity, unnecessary indirection, or verbose AI-shaped code. Prefer narrow functions, specific names, and obvious control flow.
- Comments should be rare and useful. Explain non-obvious intent, not the obvious mechanics.

## Changelog Discipline

- Keep `CHANGELOG.md` current for every substantial pass.
- Add new entries to the top of the `Unreleased` section, not the bottom.
- Track the things that matter at commit time: user-visible changes, runtime/architecture changes, honest limitations, and meaningful validation or soak results.
- Update the changelog in the same pass as the code change, not later.
- Use `CHANGELOG.md` as the source for commit messages, release notes, and publish summaries.

## Validation Standard

- Do not stop at `cargo test` if the change affects runtime behavior.
- For CLI/runtime work: run targeted command smoke plus workspace tests and clippy.
- For replay/capture/decoder/session changes: run replay soak or representative offline smoke.
- For live RF or hardware changes: use the attached RTL-SDR on `athena` when relevant and verify on real signal paths, not just synthetic tests.
- For GUI/TUI work: run build/type checks and a real runtime smoke.
- If visuals changed in a substantial way, take screenshots during the pass, review the actual result, and iterate.
- README and support docs must state real shortcomings plainly. Do not overclaim decoder coverage, hardware support, or live validation.

## UI / Visual Work

- Take screenshots during substantial UI passes, not just at the end.
- Review actual runtime behavior instead of trusting code changes.
- Prioritize feel, clarity, spacing, hit targets, and fast intentional motion.
- Call out ugly, broken, primitive, inconsistent, or AI-slop visuals plainly when they are actually there, then fix them.
- If the behavior could differ across displays, check it on more than one layout instead of assuming.

## Stable User Preferences

- Keep the README human. Explain what the app does for new SDR users and what experts should expect.
- Be candid about failures, limitations, missing decoder coverage, and backend dependencies.
- When asked to harden or publish, audit holistically: CLI, TUI, GUI, docs, packaging, soak, and live hardware behavior together.
