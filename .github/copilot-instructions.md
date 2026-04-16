# WaveRunner AI Editing Guidelines

AI-generated changes in this repository must read like deliberate maintenance by a human engineer, not like a speculative rewrite.

## Match the codebase

- Follow existing module structure, naming, and data flow before introducing new helpers or abstractions.
- Prefer extending an established type or function over adding a new layer of indirection.
- Keep diffs narrow. Fix the problem at the point of failure instead of fanning changes across unrelated files.

## Avoid low-signal AI patterns

- Do not add parameters, helpers, or abstractions that are not meaningfully used.
- Do not add comments that restate obvious code. Comments should explain non-obvious constraints, protocol details, or hardware behavior.
- Do not leave behind placeholder branches, speculative extension points, or "future-proof" wrappers with one caller.
- Do not hardcode magic values without naming the constraint they represent.
- Do not silently swallow errors when surfacing them would help an operator diagnose SDR, decoder, or toolchain problems.

## Preserve operator trust

- Be explicit about external-tool assumptions, sample rates, and hardware constraints.
- Keep decoder claims honest in code and docs. If a path is unverified or environment-limited, say so plainly.
- Prefer operator-visible diagnostics over hidden fallbacks.

## Verification is required

- Run targeted tests for touched modules.
- Run workspace linting and broader tests before finalizing substantial changes.
- Do not delete or skip failing tests to make a change pass.
- Verify new decoder behavior against real captures or live hardware when the task touches runtime signal paths.

## Dependencies and provenance

- Do not introduce new dependencies unless they are clearly necessary and actively maintained.
- Reuse existing crates and utilities in the workspace when practical.
- Treat copied or generated code as untrusted until it has been reviewed, simplified, and validated against repository conventions.
