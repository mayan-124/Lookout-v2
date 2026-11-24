# Working rules

- Do not run `gh run watch` or wait for CI. Push, report the commit hash, stop. The owner checks CI.
- Run fmt, clippy and tests once, before pushing. After small edits run only the affected tests.
- Do not run benchmarks or memory measurements unless the task says to.
- Do not paste logs or file contents in reports. Final report: 20 lines max: what works, failures, what you could not verify, commit hash.
- Do not read files you do not need. Prefer targeted searches.
- Build only the current phase. Do not add features, tests or docs that were not asked for.
- CI only runs on PRs and manual dispatch; before every push run the exact commands from ci.yml locally.
