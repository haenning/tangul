# Testing

[![tests](https://github.com/haenning/tangul/raw/badges/tests.svg)](https://github.com/haenning/tangul/blob/badges/summary.md)
[![coverage](https://github.com/haenning/tangul/raw/badges/coverage.svg)](https://github.com/haenning/tangul/blob/badges/summary.md)
[![databases](https://github.com/haenning/tangul/raw/badges/databases.svg)](#live-database-tests)

How tangul is tested, what the numbers mean, and which tests back each promise the
[README](../README.md) makes. The badges and the
[full report](https://github.com/haenning/tangul/blob/badges/summary.md) (tests and
coverage per crate and file) come from the latest CI run on `main`.

## The layers

| Layer | Where | What it checks |
|---|---|---|
| Unit | next to the code (`#[cfg(test)]`) | logic without I/O: the SQL guard, dialects, the AI's view of rows, boards, settings |
| Live database | `crates/engine/tests/live.rs` | the real engine against real Postgres, MySQL and an SSH bastion: read-only mode, time limits, what readers may run |
| Engine without a server | `crates/engine/tests/local.rs` | the engine and its gate on SQLite |
| AI tools | `crates/ai/tests/` | every tool through the registry, as a model calls it, on hand-built schemas and real SQLite files |
| Model providers | `crates/llm/tests/` | each provider's requests and recorded streams, over HTTP |
| Headless UI | `crates/app/src/workspace/ui_tests.rs` | the real workspace in a window without a screen, driven by clicks and keys |
| Adversarial | in the layers above | writes hidden in reads, renamed columns, comments the server reads differently: each must fail |

## Running them

```sh
make test               # everything without databases (a minute or two)
make test-live          # plus the live tests (Docker: docker-compose.yml)
make test-live-native   # the same without Docker (Debian/Ubuntu, installs via apt)
```

CI (`.github/workflows/ci.yml`) runs all of it once per push to `main` and per
pull request, on Linux: every crate's tests, including the live and the UI tests, in
one `cargo nextest` run that also measures coverage (`cargo llvm-cov`). Format,
clippy and the dependency checks run beside it. Releases run the same before they
build. Nothing runs twice; a run takes about 5 to 10 minutes with a warm cache.

## Coverage

Lines covered, by `cargo llvm-cov`, over all tests including the live and UI tests.

- **Safety and I/O crates** (core, drivers, engine, assist, tunnel, data, service,
  llm, ai) aim for **95 %**. They hold the rules that keep data safe, so they carry
  the badge.
- **The app** (`crates/app`) is measured and reported on its own, without a target:
  its UI tests drive the real workspace, but drawing itself isn't asserted.

Not measured: what only runs on macOS (the Keychain, the app bundle, Metal drawing),
since CI runs on Linux. The release build compiles it.

## Live database tests

`crates/engine/tests/live.rs` runs against the database versions in
`docker-compose.yml` (the badge shows them) and through the SSH bastion there. Without
`TANGUL_TEST_PG` / `TANGUL_TEST_MYSQL` / `TANGUL_TEST_SSH` they are skipped, so
`make test` works without databases.

## What backs each promise

Each promise in the README, and the tests that would fail if it broke. CI checks that
every test named here exists (`scripts/check-testing-doc.py`).

| Promise | Tests |
|---|---|
| Read-only mode: the server session refuses writes | `crates/engine/tests/live.rs::read_only_checks`, `crates/engine/src/gate.rs::the_gate_enforces_for_users`, `crates/assist/src/guard.rs::switching_off_read_only_counts_as_a_write` |
| A write never runs past read-only mode; `UPDATE`/`DELETE` without `WHERE` asks first | `crates/assist/src/guard.rs::writes_are_found`, `crates/assist/src/guard.rs::missing_where_is_found`, `crates/assist/src/guard.rs::every_statement_of_a_script_is_checked`, `crates/engine/src/gate.rs::users_write_in_write_mode` |
| Read-only mode switches itself back on | `crates/data/src/settings.rs::relock_after` |
| A time limit for every query, set on the server | `crates/engine/tests/live.rs::time_limit_checks` |
| Statements for selected rows are keyed by primary key, or say why not | `crates/assist/src/edit.rs::by_primary_key`, `crates/assist/src/edit.rs::falls_back_to_all_columns_with_a_warning` |
| Secrets stay in the Keychain | `crates/data/src/profiles.rs::secrets_of_the_app_are_kept_and_forgotten` |
| Passwords in SQL never reach history, bookmarks, chats or the audit log | `crates/assist/src/redact.rs::hides_passwords` |
| SSH: unknown host keys ask first | `crates/engine/tests/live.rs::ssh_tunnel`, `crates/tunnel/src/lib.rs::host_key_messages` |
| Schedules, boards and the editor only read | `crates/engine/src/gate.rs::schedules_boards_and_the_editor_only_read`, `crates/engine/tests/live.rs::board_checks`, `crates/engine/tests/live.rs::editor_checks` |
| The AI only reads: one statement, exactly as checked, rolled back; a write hidden in a read fails | `crates/engine/tests/live.rs::assistant_checks`, `crates/engine/src/gate.rs::readers_send_one_statement`, `crates/engine/src/gate.rs::readers_send_what_was_checked`, `crates/assist/src/guard.rs::one_read_at_a_time` |
| The AI never runs a change; you approve it | `crates/engine/src/gate.rs::assistant_writes_are_proposals`, `crates/engine/tests/local.rs::assistant_writes_are_proposed_never_run_and_audited`, `crates/ai/tests/sqlite.rs::writes_are_never_run`, `crates/ai/tests/sqlite.rs::a_write_asked_for_by_the_model_is_refused_and_nothing_changes`, `crates/ai/tests/sqlite.rs::proposals_open_for_the_user_and_never_run` |
| The AI uses its own user, never yours instead | `crates/app/src/workspace/ui_tests.rs::a_missing_ai_user_is_shown_never_replaced_by_yours`, `crates/app/src/workspace/ui_tests.rs::a_failed_ai_connection_is_shown_never_switched` |
| Schema only: the AI never sees a row | `crates/ai/tests/sqlite.rs::schema_only_sees_structure_and_never_a_row` |
| Redacted and hidden columns, renamed or computed, stay so for the AI | `crates/assist/src/ai_view.rs::renamed_and_computed_columns_count_as_their_sources`, `crates/assist/src/ai_view.rs::ctes_subqueries_and_unions_pass_on_what_they_use`, `crates/assist/src/ai_view.rs::generated_columns_and_views_carry_what_they_come_from`, `crates/assist/src/ai_view.rs::block_queries_keeps_redacted_columns_out_of_conditions`, `crates/ai/tests/sqlite.rs::redacted_and_hidden_columns_as_ai_tools_see_them`, `crates/core/src/ai_view.rs::unknown_or_outdated_keys_fail_instead_of_opening`, `crates/core/src/ai_view.rs::a_pattern_without_a_dot_or_with_too_many_is_refused`, `crates/data/src/database_settings.rs::versions_unknown_keys_and_newer_layouts` |
| Boards by AI: tiles only read, only at the revision read, heavy ones wait | `crates/service/src/boards.rs::an_ai_writes_tiles_that_read_and_leaves_the_rest_to_the_user`, `crates/service/src/boards.rs::an_ai_changes_a_board_only_at_the_revision_it_read`, `crates/service/src/boards.rs::a_query_the_plan_objects_to_waits_until_the_user_ran_it` |
| An update installs only when it is the same app, signed with the same certificate, of the version announced, from this repository's releases; anything else leaves the app as it was | `crates/update/src/install/attacks.rs::only_an_app_signed_with_the_same_certificate_as_the_same_app_is_installed` (macOS job), `crates/update/src/install.rs::the_designated_requirement_or_none_for_ad_hoc`, `crates/update/src/install.rs::a_failed_swap_puts_the_running_app_back`, `crates/update/src/release.rs::only_published_releases_with_the_dmg` |
| Reads for others give up waiting for a lock | `crates/engine/tests/live.rs::lock_wait_checks` |
