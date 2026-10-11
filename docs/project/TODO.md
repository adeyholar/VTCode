Audit the codebase for oversized files, monolithic modules, duplicated logic, and tightly coupled components. Identify refactoring opportunities, extract reusable components, and improve modularity and separation of concerns. Prioritize by complexity, duplication, maintainability, and impact. Produce a phased refactoring plan with file references, proposed module boundaries, and risks. Follow KISS/DRY, preserve existing behavior and public APIs, and avoid unnecessary abstractions.

===

Evidence from wc -l + grep (excluding target/):

- vtcode-core/src: 829 files, ~272k LOC, lib.rs:119 exports ~70 pub mod. Mixes llm/tools/config/prompts/core/agent/scheduler/skills/safety/compaction/session.
- src/agent: 537 files, ~195k LOC. src/agent/runloop/unified/ nests 6-7 deep: turn/tool_outcomes/handlers/guards/.
- vtcode-llm/src/providers: 208 files, ~86k LOC.
- vtcode-ui/src/tui/core_tui/session: ~19k LOC across 30+ files.
- Largest non-test sources:
- src/agent/runloop/unified/turn/session_loop_runner/orchestration/mod.rs:2874
- src/agent/runloop/unified/state.rs:2366
- src/agent/runloop/unified/turn/turn_loop.rs:2398 + turn_loop/post_tool_recovery.rs:2024
- src/agent/runloop/unified/turn/turn_processing/llm_request/request_builder.rs:1951
- crates/codegen/vtcode-core/src/core/agent/events/mod.rs:1995 + lifecycle.rs:1887
- crates/codegen/vtcode-core/src/utils/session_archive.rs:1986
- crates/codegen/vtcode-core/src/tool_policy.rs:1913
- crates/codegen/vtcode-core/src/skills/loader.rs:1870
- crates/codegen/vtcode-core/src/tools/handlers/read_file.rs:1848 vs tools/file_ops/read.rs:1655
- crates/codegen/vtcode-core/src/compaction/memory_envelope.rs:1754
- crates/codegen/vtcode-core/src/scheduler/mod.rs:1637
- crates/codegen/vtcode-core/src/core/agent/runner/execute.rs:1935
- crates/codegen/vtcode-ui/src/tui/core_tui/session/input.rs:2028 + input_manager.rs:1404 + modal/state.rs:1691
- crates/codegen/vtcode-llm/src/providers/openai/request_builder.rs:1760 + shared/responses_adapter.rs:1524 + ollama.rs:1549

1. Oversized / Monolithic
   Rank File Problem
   P0 vtcode-core/src/lib.rs + tools/mod.rs + tools/registry/ (executors.rs:1564, execution_kernel.rs:1524, execution_facade.rs:1391, builtins.rs:1173, cgp_facade.rs:1046) God crate + facade proliferation. cgp_facade vs policy_facade vs tool_catalog_facade vs execution_facade = 4 thin wrappers over same registry.
   P0 state.rs:2366 God State: SessionStats holds CircuitBreaker + HealthTracker + RateLimiter + ValidationCache directly; imports 6 vtcode_core::\* subsystems. Every turn touches it.
   P0 orchestration/mod.rs:2874, support.rs:1790, turn/turn_loop.rs:2398 Orchestrator god: imports config + runtime + steering + hooks + llm + session_archive + UI + git + model_picker + palettes + planning. Holds session bootstrap, handoff, checkpoint, metrics, turn-tail.
   P1 scheduler/mod.rs:1637 Single file: cron parse + humantime + BTreeMap store + launchd/systemd install + jitter/claim + notifications + tokio::process.
   P1 skills/loader.rs:1870, tool_policy.rs:1913, session_archive.rs:1986 Each 3 concerns in 1 file. Loader: discovery + validation + CLI-bridge + container + system-install.
   P1 core/agent/events/mod.rs:1995 + lifecycle.rs:1887 + src/.../ui_interaction_stream.rs:1830 + inline_events/harness.rs:1172 Event plumbing split across 2 crates with no single ThreadEvent-adjacent boundary.
   P2 session/input.rs:2028, modal/state.rs:1691, startup/mod.rs:1537 UI input state machine + keymap + completion + clipboard in one.
2. Duplicated Logic (DRY violations)
3. Read path x2: tools/handlers/read_file.rs:1848 (ReadFileHandler, ReadRange, LineRecord) vs tools/file_ops/read.rs:1655 + read/{segments.rs,legacy.rs,tool.rs} + tools/read_limits.rs + registry/{spool_processing,output_processing,execution_kernel}.rs. Same read_limits/output_limits checks in 8 files.
4. LLM request building: vtcode-llm/providers/openai/request_builder.rs:1760 vs anthropic/request_builder/ vs shared/responses_adapter.rs:1524 vs openai_compat.rs vs common/http.rs vs src/.../llm_request/request_builder.rs:1951 vs copilot_runtime.rs:1297. Each provider reimplements headers/retry/tool-format/token-count.
5. Tool execution x3: src/.../unified/tool_routing/ (approval, permission_prompt:1648, shell_approval, preflight) vs tool_pipeline/ (execution_run, validation, hitl, file_conflict) vs turn/tool_outcomes/ (dispatch, handlers, guards/read_guard:1153, helpers) vs vtcode-core/tools/registry/\*. Approval recorded in both tool_routing/approval_cache.rs + persistence.rs and registry/approval_recorder.rs:463.
6. Compaction x3: vtcode-core/compaction/{strategy,summarize,two_pass,auto,prefire,continuity,memory_envelope:1754} vs core/agent/compaction_checkpoint.rs vs src/.../turn/compaction/{mod,memory_envelope,progress_ui,recovery_preview} vs turn_loop/recovery_compaction.rs + model_switch_compaction.rs.
7. Permission/prompt: tool_call_safety.rs + tool_routing/permission_prompt.rs + tool_pipeline/validation.rs + handlers/guards/\* all match on tool name + workspace trust.
8. Tests as smell: turn_loop/tests.rs:3957, tool_outcomes/helpers/tests.rs:3310, subagents/tests.rs:3607, registry/tests.rs:2878 — monoliths only testable via 2-3k line fixtures.
9. Tight Coupling

- state.rs -> compaction + config + harness_kernel + request_envelope + llm::provider + llm::request_gap + llm::usage_cost + tools::{circuit_breaker,health,rate_limiter,validation_cache}. Change to any tool health policy recompiles session state.
- turn_loop.rs -> vtcode_core::{acp,config,agent::events,refusal,runtime,decision_tracker,trajectory,exec::events,hooks,llm,tools,utils::ansi} + vtcode_ui::{InlineHeaderContext,InlineHandle,InlineSession} + 4 local unified::\* subtrees. UI + core + runloop fused; can't unit-test loop without TUI.
- orchestration/mod.rs -> vtcode_commons + vtcode_config + vtcode_core(6 mods) + vtcode_ui + crate::agent::runloop::{git,model_picker,palettes,planning,postamble,session_setup}. No seam for archive/handoff/metrics/plan_seed/support/turn_tail — all super::\*.
- vtcode-core allows clippy::large_futures at crate root — hides oversized async state machines in runner/execute.rs:1935, runtime/mod.rs:1636.
  Phased Plan (KISS, no new crates until Phase 4)
  Phase 0 — Guardrails (no behavior change):
- Add cargo check --locked, ast-grep scan, file-size budget test (fail >1500 LOC non-test, >800 LOC tools/registry/_). Extract test fixtures: move _\_tests.rs >1500 helpers to test_support.rs.
- Baseline: nextest -p vtcode-core -p vtcode --profile quick.
  Phase 1 — Low-risk splits (1 file -> N modules, same pub API):

1. scheduler/mod.rs:1637 -> scheduler/{schedule_store.rs, cron_parse.rs, service_install.rs, run_queue.rs}. Keep mod.rs re-exports.
2. utils/session_archive.rs:1986 -> {archive_io.rs, archive_index.rs, message_codec.rs}.
3. tool_policy.rs:1913 -> {policy_model.rs, policy_eval.rs}; skills/loader.rs:1870 -> keep facade, move discovery.rs / validation.rs bodies out (already exist, just delegate).
4. prompts/guidelines.rs:1540 + resources.rs:817 + few_shot.rs:691 -> no split, just dedupe static_prompts.rs:211 vs runtime_guidance.rs:184 vs runtime_contract.rs:183.

- Risk: Low. Verify: re-export check + nextest -E 'test(scheduler|archive|policy)'.
  Phase 2 — DRY read + LLM builder (highest ROI):

1. Single ReadExtent {path, range, limit_policy} in tools/file_ops/read/extent.rs; make handlers/read_file.rs a thin Tool adapter over file_ops/read.rs. Delete LineRecord dup, unify read_limits.rs + output_limits.rs into one limits.rs. Preserve ReadFileArgs shape.
2. Extract vtcode-llm/src/providers/shared/request_core.rs: headers + retry-after + tool-format + token-budget. Migrate openai/request_builder.rs first, then anthropic/compat.rs, then ollama.rs:1549, openrouter.rs, zai.rs. Keep provider mod.rs API unchanged.

- Risk: Medium — output truncation / cache-control regressions. Mitigate with asymmetric boundary tests (empty file, 1-line-over-limit, binary, symlink).
  Phase 3 — Unify tool execution:

1. Don't merge 3 dirs at once. First extract reusable ApprovalDecision{allow,deny,ask} + PreflightReport value objects shared by tool_routing/preflight.rs, tool_pipeline/validation.rs, registry/policy.rs:741.
2. Collapse registry/{execution_facade,cgp_facade,policy_facade,tool_catalog_facade}.rs into one registry/facade.rs re-exporting existing fns. Delete pass-throughs.
3. Then narrow src/.../tool_routing/ to policy only, tool_pipeline/ to exec only, tool_outcomes/ to rendering only; shared guard read_guard.rs:1153/spool_guard moves to vtcode-core/tools/guards/.

- Risk: High — permission bypass. Require serial_test approval tests + manual HITL matrix before/after.
  Phase 4 — Break gods (only after 2+3):

1. state.rs:2366 -> state/{session_stats.rs, followup.rs, auto_permission.rs, resilient_handles.rs}. Inject Arc<dyn Health> traits instead of concrete fields; keep pub(crate) constructors.
2. orchestration/mod.rs:2874 -> keep mod.rs as 200-line dispatcher; move handoff.rs, archive.rs, metrics.rs, plan_seed.rs, turn_tail.rs to free fns taking explicit &SessionState + &AgentRuntime params (no super::support::\* glob).
3. turn_loop.rs:2398 + post_tool_recovery.rs:2024 -> turn_loop/{loop_policy.rs, recovery.rs, accounting.rs}. Decouple UI: introduce TurnEvents: Fn(HarnessEvent) trait so turn_loop doesn't import vtcode_ui directly.
4. Only if still >500 LOC/file or circular deps: consider extracting vtcode-scheduler / vtcode-compaction crates. Default: don't — new crates add version churn per adding-workspace-crate skill.

- Risk: Highest — lifecycle/ordering bugs. Mitigate with pty_tests, inline_events harness tests, checkpoint golden tests.
  Suggested order by impact/effort: Phase1(scheduler/archive) -> Phase2(read) -> Phase2(llm-core) -> Phase3(facade collapse) -> Phase3(approval value object) -> Phase4(state/orchestration).
  What NOT to do: generic Executor<T> / Provider<T> abstractions, new workspace crates for every tool, renaming public ToolRegistry/AgentRuntime APIs — all violate KISS and break downstream vtcode-acp/mcp/a2a.
