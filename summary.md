## Tests

**581 passed, 0 failed** · live against postgres 16, mysql 8.4 and an SSH bastion

## Coverage (lines)

**Safety and I/O crates: 95.7%** (target 95%) · app: 71.4% · all: 84.4%

| Crate | Tests | Lines | Covered | |
|---|---:|---:|---:|---|
| ai | 51 | 2070 | 97.2% | ✅ |
| assist | 194 | 10749 | 96.4% | ✅ |
| core | 19 | 824 | 95.6% | ✅ |
| data | 82 | 3905 | 92.5% | ⚠️ |
| drivers | 7 | 1242 | 94.5% | ⚠️ |
| engine | 16 | 1352 | 93.1% | ⚠️ |
| llm | 29 | 1338 | 99.8% | ✅ |
| service | 29 | 1602 | 99.9% | ✅ |
| tunnel | 2 | 235 | 66.8% | ⚠️ |
| app | 143 | 18925 | 71.4% |  |
| update | 9 | 564 | 58.0% |  |

<details><summary>Files</summary>

| File | Lines | Covered |
|---|---:|---:|
| ai/src/audit.rs | 5 | 80.0% ⚠️ |
| ai/src/chat/mod.rs | 249 | 98.8% |
| ai/src/chat/prompt.rs | 62 | 100.0% |
| ai/src/chat/text.rs | 122 | 100.0% |
| ai/src/ctx.rs | 226 | 96.9% |
| ai/src/features.rs | 15 | 100.0% |
| ai/src/mcp.rs | 142 | 97.9% |
| ai/src/results.rs | 160 | 98.8% |
| ai/src/runtime.rs | 52 | 92.3% ⚠️ |
| ai/src/tool.rs | 117 | 99.1% |
| ai/src/tools/boards.rs | 195 | 86.2% ⚠️ |
| ai/src/tools/bookmarks.rs | 16 | 100.0% |
| ai/src/tools/catalog.rs | 36 | 100.0% |
| ai/src/tools/cells.rs | 54 | 96.3% |
| ai/src/tools/connections.rs | 37 | 94.6% ⚠️ |
| ai/src/tools/describe.rs | 205 | 99.5% |
| ai/src/tools/features.rs | 12 | 100.0% |
| ai/src/tools/joins.rs | 37 | 100.0% |
| ai/src/tools/mod.rs | 42 | 100.0% |
| ai/src/tools/profile.rs | 62 | 95.2% |
| ai/src/tools/propose.rs | 22 | 100.0% |
| ai/src/tools/query.rs | 149 | 99.3% |
| ai/src/tools/search.rs | 34 | 100.0% |
| ai/src/tools/slice.rs | 19 | 100.0% |
| app/src/ai_settings/assistant_page.rs | 179 | 43.0% |
| app/src/ai_settings/layout.rs | 376 | 75.0% |
| app/src/ai_settings/mcp_page.rs | 93 | 92.5% |
| app/src/ai_settings/mod.rs | 260 | 49.6% |
| app/src/assets.rs | 21 | 28.6% |
| app/src/assistant/mod.rs | 193 | 77.2% |
| app/src/assistant/view.rs | 338 | 76.9% |
| app/src/board/form.rs | 679 | 49.2% |
| app/src/board/mod.rs | 321 | 80.4% |
| app/src/board/view.rs | 722 | 66.6% |
| app/src/bookmark_form.rs | 129 | 0.0% |
| app/src/chart.rs | 276 | 60.1% |
| app/src/completion.rs | 127 | 29.9% |
| app/src/connection.rs | 15 | 40.0% |
| app/src/connection_form.rs | 483 | 84.1% |
| app/src/file_tab.rs | 520 | 83.5% |
| app/src/main.rs | 120 | 37.5% |
| app/src/monitor.rs | 37 | 56.8% |
| app/src/palette.rs | 171 | 100.0% |
| app/src/query_router.rs | 37 | 73.0% |
| app/src/query_tab/editor.rs | 454 | 86.6% |
| app/src/query_tab/hints.rs | 36 | 100.0% |
| app/src/query_tab/mod.rs | 565 | 80.4% |
| app/src/result_view/filter.rs | 252 | 74.2% |
| app/src/result_view/grid.rs | 2048 | 79.8% |
| app/src/result_view/mod.rs | 899 | 69.2% |
| app/src/result_view/xlsx.rs | 59 | 96.6% |
| app/src/settings.rs | 93 | 33.3% |
| app/src/syntax.rs | 110 | 100.0% |
| app/src/tabs.rs | 162 | 92.6% |
| app/src/theme.rs | 509 | 91.2% |
| app/src/ui.rs | 299 | 93.0% |
| app/src/workspace/ai.rs | 471 | 93.6% |
| app/src/workspace/ai_settings.rs | 195 | 69.7% |
| app/src/workspace/assist.rs | 52 | 82.7% |
| app/src/workspace/assistant.rs | 164 | 75.6% |
| app/src/workspace/bars.rs | 679 | 70.5% |
| app/src/workspace/boards.rs | 977 | 79.9% |
| app/src/workspace/bookmarks.rs | 342 | 27.8% |
| app/src/workspace/command_palette.rs | 183 | 56.3% |
| app/src/workspace/connections.rs | 687 | 60.0% |
| app/src/workspace/database_tabs.rs | 133 | 93.2% |
| app/src/workspace/empty_state.rs | 112 | 92.9% |
| app/src/workspace/history.rs | 307 | 61.2% |
| app/src/workspace/mcp.rs | 123 | 85.4% |
| app/src/workspace/mod.rs | 326 | 78.8% |
| app/src/workspace/perf_boards.rs | 141 | 33.3% |
| app/src/workspace/quick_values.rs | 52 | 26.9% |
| app/src/workspace/remove_database.rs | 158 | 89.2% |
| app/src/workspace/safety.rs | 159 | 7.5% |
| app/src/workspace/scheduled.rs | 178 | 5.1% |
| app/src/workspace/schema.rs | 822 | 66.5% |
| app/src/workspace/sidebar.rs | 208 | 87.0% |
| app/src/workspace/tab_values.rs | 179 | 88.3% |
| app/src/workspace/tabs.rs | 704 | 58.2% |
| app/src/workspace/update.rs | 157 | 38.2% |
| app/src/workspace/value_panel.rs | 629 | 90.5% |
| app/src/workspace/value_window.rs | 204 | 86.8% |
| assist/src/ai_rules.rs | 366 | 97.5% |
| assist/src/ai_view.rs | 953 | 97.1% |
| assist/src/analyze.rs | 239 | 100.0% |
| assist/src/chart.rs | 477 | 99.6% |
| assist/src/columns.rs | 502 | 96.4% |
| assist/src/complete.rs | 1177 | 93.8% ⚠️ |
| assist/src/copy.rs | 162 | 100.0% |
| assist/src/dialect/mod.rs | 181 | 88.4% ⚠️ |
| assist/src/dialect/mysql.rs | 225 | 92.0% ⚠️ |
| assist/src/dialect/mysql/perf.rs | 314 | 100.0% |
| assist/src/dialect/postgres.rs | 241 | 96.7% |
| assist/src/dialect/postgres/perf.rs | 328 | 100.0% |
| assist/src/dialect/sqlite.rs | 94 | 53.2% ⚠️ |
| assist/src/digest.rs | 146 | 99.3% |
| assist/src/edit.rs | 297 | 98.7% |
| assist/src/export.rs | 41 | 100.0% |
| assist/src/format.rs | 129 | 100.0% |
| assist/src/grammar.rs | 488 | 85.5% ⚠️ |
| assist/src/guard.rs | 347 | 98.6% |
| assist/src/hints.rs | 136 | 99.3% |
| assist/src/join_graph.rs | 258 | 96.9% |
| assist/src/json.rs | 378 | 97.4% |
| assist/src/lexer.rs | 290 | 99.7% |
| assist/src/mark.rs | 572 | 100.0% |
| assist/src/nl.rs | 157 | 100.0% |
| assist/src/perf.rs | 151 | 98.7% |
| assist/src/profile.rs | 53 | 100.0% |
| assist/src/redact.rs | 86 | 100.0% |
| assist/src/refine.rs | 436 | 92.4% ⚠️ |
| assist/src/related.rs | 281 | 94.3% ⚠️ |
| assist/src/schema.rs | 185 | 100.0% |
| assist/src/slice.rs | 325 | 98.2% |
| assist/src/statement.rs | 223 | 99.6% |
| assist/src/suggest.rs | 163 | 99.4% |
| assist/src/usage.rs | 175 | 98.9% |
| assist/src/value.rs | 173 | 98.8% |
| core/src/ai_view.rs | 155 | 100.0% |
| core/src/board.rs | 281 | 99.6% |
| core/src/chart.rs | 62 | 98.4% |
| core/src/lib.rs | 27 | 77.8% ⚠️ |
| core/src/llm.rs | 52 | 100.0% |
| core/src/pinned.rs | 21 | 95.2% |
| core/src/profile.rs | 111 | 89.2% ⚠️ |
| core/src/result.rs | 102 | 90.2% ⚠️ |
| core/src/usage.rs | 13 | 61.5% ⚠️ |
| data/src/boards.rs | 203 | 97.0% |
| data/src/config.rs | 36 | 58.3% ⚠️ |
| data/src/database_settings.rs | 96 | 99.0% |
| data/src/lib.rs | 190 | 87.9% ⚠️ |
| data/src/local/audit.rs | 102 | 93.1% ⚠️ |
| data/src/local/bookmarks.rs | 178 | 96.1% |
| data/src/local/chats.rs | 136 | 88.2% ⚠️ |
| data/src/local/favorites.rs | 40 | 90.0% ⚠️ |
| data/src/local/history.rs | 180 | 93.9% ⚠️ |
| data/src/local/kv.rs | 65 | 95.4% |
| data/src/local/mod.rs | 114 | 90.4% ⚠️ |
| data/src/local/schema_cache.rs | 62 | 98.4% |
| data/src/local/usage.rs | 33 | 90.9% ⚠️ |
| data/src/metrics.rs | 208 | 88.9% ⚠️ |
| data/src/migrations.rs | 207 | 96.6% |
| data/src/profiles.rs | 334 | 85.0% ⚠️ |
| data/src/secrets.rs | 280 | 88.9% ⚠️ |
| data/src/settings.rs | 619 | 91.8% ⚠️ |
| data/src/themes.rs | 672 | 97.9% |
| data/src/upgrade.rs | 150 | 95.3% |
| drivers/src/lib.rs | 22 | 68.2% ⚠️ |
| drivers/src/mysql.rs | 368 | 95.7% |
| drivers/src/postgres.rs | 344 | 94.8% ⚠️ |
| drivers/src/sqlite.rs | 508 | 94.7% ⚠️ |
| engine/src/api.rs | 17 | 82.4% ⚠️ |
| engine/src/audit.rs | 51 | 98.0% |
| engine/src/engine.rs | 379 | 91.3% ⚠️ |
| engine/src/gate.rs | 340 | 97.1% |
| engine/src/lib.rs | 82 | 89.0% ⚠️ |
| engine/src/pool.rs | 104 | 98.1% |
| engine/src/run.rs | 338 | 94.4% ⚠️ |
| engine/src/schema.rs | 41 | 61.0% ⚠️ |
| llm/src/anthropic.rs | 290 | 99.7% |
| llm/src/gemini.rs | 262 | 100.0% |
| llm/src/http.rs | 171 | 100.0% |
| llm/src/lib.rs | 74 | 98.6% |
| llm/src/ollama.rs | 177 | 100.0% |
| llm/src/openai.rs | 270 | 99.6% |
| llm/src/types.rs | 94 | 100.0% |
| service/src/boards.rs | 788 | 99.9% |
| service/src/connections.rs | 197 | 99.5% |
| service/src/knowledge.rs | 350 | 100.0% |
| service/src/perf.rs | 51 | 100.0% |
| service/src/schedule.rs | 89 | 100.0% |
| service/src/values.rs | 127 | 100.0% |
| tunnel/src/lib.rs | 235 | 66.8% ⚠️ |
| update/src/install.rs | 320 | 51.6% |
| update/src/release.rs | 192 | 57.3% |
| update/src/version.rs | 52 | 100.0% |

</details>
