# `prmarmot-cli watch --until ci-pass` with real exit codes
category: Ideas

Today: `prmarmot-cli watch --pr` streams every change as NDJSON until the PR merges or closes, with `--events N` to stop early; exit codes only distinguish errors (gh missing, rate limited), not outcomes, and there is no timeout.

`prmarmot-cli watch --pr owner/name#123 --until ci-pass|approved|mergeable|merged --timeout 30m` would block and exit 0 when the condition is met, a distinct code when a terminal failure happens (CI red, changes requested, closed) and another on timeout. One process, one GraphQL request per poll, no rate-limit-burning loops for scripts and coding agents.

Vote with 👍. Comment with the conditions you would wait for.
