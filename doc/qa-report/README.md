# QA reports

Point-in-time code-quality reviews of the MAKI codebase. Each report
records the state at a given release, the findings by severity, and
which batches of fixes landed. They are historical documents: once a
report is closed it moves to `archive/` and is not updated further.

| Report | State at | Status |
|---|---|---|
| `archive/qa-report-2026-04-17.md` | commit `855ad50` (post P1a) | Closed — superseded by the 2026-05-03 report |
| `archive/qa-report-2026-05-03.md` | v4.4.14 | Closed 2026-10-19 (see below) |

## Closing status of the 2026-05-03 report (as of 2026-10-19, v4.12.1)

All three batches landed on 2026-05-03/04 (released in v4.4.15): Batch 1 small
DRY wins (`6889825`), Batch 2 structural splits (`85984f8`, `7ce8d11`,
`9d24d8f`, `6262a39`, `ae3bd4d`, `6f74dec`), Batch 3 documentation
polish (`0eb33c6`). H6 (no inline tests in `main.rs`) was explicitly
not addressed; M5 was withdrawn.

The two items the report left as "opportunistic cleanup as touched",
measured on 2026-10-19:

- **H5 — raw `spawn_blocking` in web handlers.** 52 call sites in
  `src/web/routes/` now use `spawn_catalog_blocking`; 53 still call
  `tokio::task::spawn_blocking` directly, across 17 files (the helper's
  own body in `routes/mod.rs` excluded). Largest remaining clusters:
  `ai/faces.rs` (12), `media.rs` (9), `volumes.rs` (6), `stacks.rs` (6).
  Roughly half of the 106 sites the report counted have migrated.
- **H1 — remaining `run_command` match arms in `src/main.rs`.** The
  Describe (~line 2594), Cleanup (~2859) and GeneratePreviews (~2931)
  arms are still inline; `main.rs` is 3294 lines. Collection and
  SavedSearch have since moved to `src/commands/`.

No further batches are planned for this report. Both carryovers remain
opportunistic — migrate a site or lift an arm when the surrounding
code is touched anyway — unless a new QA pass is requested.
