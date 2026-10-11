# Compaction Engine

How VT Code keeps long sessions inside the model window. See `crates/codegen/vtcode-core/src/compaction/mod.rs` for the
implementation.

## Request history and local clearing

Session history and provider request history have separate lifetimes. The shared
`normalize_history_for_request_shared` helper repairs pairing and order on a request projection, retaining the shared
fast path when history is already valid. `clear_old_tool_results` then replaces eligible old results in that projection.
Neither operation rewrites original outputs in session history or canonical `ThreadEvent` records. Actual compaction
replaces live history and records a recoverable full-history artifact and `thread.compact_boundary`.

The existing `agent.harness.tool_result_clearing` policy is enabled by default. Its configured trigger is 40,000 tokens
and `keep_tool_uses` is 2. For local clearing, the runtime resolves:

```text
history_budget = capacity - reserved_output - system_and_tool_schema_overhead
local_trigger = min(configured_trigger, max(1, floor(history_budget * 3 / 5)))
```

Subtractions saturate at zero. `capacity` is the intersection of positive provider and session limits; a zero session
ceiling means automatic capacity resolution. If capacity remains unknown, the configured trigger is preserved. For a
32,000-token capacity with 4,000 tokens reserved for output and 2,000 for the prefix, the local cap is 15,600 tokens.
Lower configured triggers still win. `enabled = false` opts out; native edit thresholds retain their configured values.

Retention starts with the newest `keep_tool_uses` results and expands to all results from their originating assistant
tool-call messages. For batches of 1, 4, and 3 results, keeping 2 protects the entire newest batch of 3; keeping 4 protects
both newest batches, totaling 7 results. Call-ID matching handles reordered completions and intervening system messages.
Unmatched results retain the count-based policy. Small results stay unchanged when a stub would not reduce estimated
tokens. Paired arguments and signatures are retained for protected results; input clearing applies only to results
actually replaced and cannot increase the total projected estimate. Reapplying clearing is idempotent.

Interactive requests use local clearing when they will not carry Anthropic native context edits. Headless `vtcode exec`
requests attach no native context management, so provider capability alone never suppresses their local fallback. This
request policy is distinct from the native inline compaction edit ladder described below.

## Headless staging and module ownership

Before estimating pressure, the headless runner synchronizes pending conversation messages into live history, normalizes
the request projection, and applies local clearing. Automatic readiness and preflight diagnostics use projected history
plus prompt overhead, with output reserved from the effective capacity. If clearing brings pressure below the trigger,
no summary call is needed. If compaction remains necessary, the engine receives full history; after a successful pass,
the runner rebuilds the projection. Failed or unchanged passes keep the existing projection. Preflight remains advisory.

| Responsibility | Source under `crates/codegen/vtcode-core/src/` |
| --- | --- |
| Pairing repair, complete-batch retention, local trigger cap | `core/agent/state.rs` |
| Synchronization, projected pressure, request assembly | `core/agent/runner/execute.rs` |
| Headless projection and compaction delegation | `core/agent/runner/summarize.rs` |
| Shared readiness, strategy orchestration, persistence | `compaction/auto.rs` |
| Local summary budgets and first-call aggregate validation | `compaction/summarize.rs` |
| Protocol-preserving input selection and preview fallback | `compaction/history_bounds.rs` |

Interactive wiring lives in `src/agent/runloop/unified/turn/turn_processing/llm_request/request_builder.rs`. Both request
paths reuse the core clearing resolver. Both runloops reuse `auto_compact_messages`; the headless caller checks its shared
readiness predicate before copy-on-write access so no-op compaction does not clone shared session history.

See the [architecture diagram](../ARCHITECTURE.md#request-projection-and-context-budgets),
[loop contract](../guides/agent-loop-contract.md#unified-auto-compaction), and
[configuration reference](../config/CONFIG_FIELD_REFERENCE.md) for the surrounding contracts.

## Strategy dispatch

`/compact` picks one strategy per provider/model (`manual_compaction_strategy`):

| Provider / model                                                        | Strategy           | How it works                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ----------------------------------------------------------------------- | ------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| OpenAI Responses models (`/responses/compact`)                          | `NativeStandalone` | Server-side standalone endpoint; returns a `response.compaction` item plus retained items, used as-is.                                                                                                                                                                                                                                                                                                                                                                                                                     |
| OpenResponses models (`/v1/responses/compact`)                          | `NativeStandalone` | Vendor-neutral standalone endpoint; retains returned messages and opaque `compaction` items for replay.                                                                                                                                                                                                                                                                                                                                                                                                                    |
| Vercel AI Gateway `openai/*` routes                                     | `NativeStandalone` | Gateway serves `POST /v1/responses/compact` (forwarded to OpenAI). Gated on `openai/` model prefix and the gateway endpoint; other routes use `Local`.                                                                                                                                                                                                                                                                                                                                                                     |
| xAI Grok models (`POST /v1/responses/compact`)                          | `NativeStandalone` | xAI serves an OpenAI-compatible compact endpoint for curated Grok models on `api.x.ai`. Gated on the model allowlist and API host; otherwise `Local`.                                                                                                                                                                                                                                                                                                                                                                      |
| Anthropic Claude Opus 4.x / Sonnet 4.6+                                 | `NativeInline`     | Threshold-triggered `compact_20260112` edit on a `generate` request with `pause_after_compaction`. Falls back to `Local` when compaction does not fire or the edit is rejected.                                                                                                                                                                                                                                                                                                                                            |
| Merge Gateway, OpenRouter, Gemini, DeepSeek, MiniMax, local, all others | `Local`            | No native compact endpoint: Merge Gateway documents no `/responses/compact` (its native `/v1/responses` shape differs from OpenAI's); OpenRouter reports compaction unsupported (its `context-compression` plugin is opt-in middle-out truncation, never enabled implicitly). Universal fallback: bounded fork summarized via the provider's one-shot response collector (which transparently consumes a normalized stream for streaming-only routes), rebuilt as summary plus retained recent messages. Works everywhere. |

## Anthropic context-edit ladder

On the `NativeInline` path VT Code sends the documented edit ladder in order — `clear_thinking_20251015` (keep 2
thinking turns), then `clear_tool_uses_20250919` (trigger 100k input tokens, keep 3 tool uses), then `compact_20260112`
(trigger floor 50k, `pause_after_compaction: true`). The clearing edits are included only when the provider reports
`supports_context_edits`; otherwise the request carries the compact edit alone. Beta headers (`compact-2026-01-12`,
`context-management-2025-06-27`) are derived from the request payload by the Anthropic provider, so callers never set
them manually. Helper: `anthropic_inline_compaction_edits`.

The response's `compaction` block is provider state, not a replacement for a local `Previous conversation summary`
message. VT Code keeps the complete raw block (including signatures, cache controls, and unknown provider fields) in the
assistant message's `reasoning_details` and replays it as the first Anthropic message on the next request. Signed blocks
are required to remain first, and thinking/redacted-thinking blocks from before that signed boundary are dropped because
Anthropic's preserved-thinking validation cannot accept them after the summarized transcript has been removed. A
threshold response whose block has `content: null` is treated as a failed native compaction and uses the local fallback.

Anthropic usage with compaction may include separate `iterations` for the compaction and message sampling passes. VT
Code aggregates recognized `message`, `fallback_message`, `advisor_message`, and `compaction` iterations for normalized
usage and cost estimates; top-level usage is used only when no recognized iteration data is present.

Anthropic's newer on-demand `compact-2026-09-04` signed-summary beta is not exposed by the current `/compact` engine.
The engine is synchronous and uses the threshold/pause strategy; adding background keep-tail swapping requires a
separate history lifecycle and beta capability contract.

## Input bounding and overflow retry

Local summaries use `local_summary_history_budget` to intersect provider capacity and a positive session ceiling. The
resolver preserves existing safety headroom (one eighth of the window plus 512 tokens), then subtracts the frozen parent
system/tool-schema prefix and any explicit summary output limit. Summary instructions also consume this allowance.
Unknown capacity keeps the existing verbatim first-fork fallback.

`bound_local_summary_history` wraps `bound_history_for_summarization`: it selects complete recent protocol groups or
bounded previews, then checks their aggregate estimate plus instructions against the resolved allowance. A positive
budget alone is insufficient because protocol metadata and minimum per-message overhead can still exceed it. Impossible
fixed inputs or aggregate minimums return a budget error before dispatch. Hierarchical summarization prepares and
validates both bands before spending either provider call.

Inline native requests use `bound_history_for_summarization`; standalone native compaction uses the marker-preserving
`bound_history_for_native_compaction`. Those paths retain their existing strategy-specific bounds. Before bounding,
local paths run `prune_oversized_tool_outputs`: `Tool` messages over 4,096 tokens are replaced with previews (call IDs
preserved, so groups stay protocol-valid) so one dump cannot evict whole turns from the fork. Flat summaries and
hierarchical bands share one capacity-retry contract (`generate_summary_with_capacity_retry`): on a context-capacity
rejection the request is retried with progressively halved budgets, skipped when a retry cannot shrink the input. When
the window is unknown (`None` budget) the first fork is verbatim, but the retry derives a fallback budget from the
failing attempt (`input_tokens / 2`) so it can still shrink instead of resending the identical fork. Empty provider
summaries fail with a diagnostic (`provider returned an empty summary`) rather than producing an empty
`Previous conversation summary:` history. Provider summary failures carry route diagnostics (`provider / model`,
`input_tokens`, `budget`); pre-dispatch budget errors report the exhausted allowance in the error chain so `/compact`
reports the actionable cause instead of only
`Failed to generate compaction summary`. Local summaries are bounded on output with
`bound_compacted_history_to_context`; native provider responses are passed through unchanged because their retained
items and opaque compaction state are the canonical next context window. The native endpoint is responsible for
returning a replayable window that fits its model context.

## Per-route policy

`CompactionRoutePolicy` (`threshold_ratio`, `retain_ratio`, `max_overflow_retries`) tunes compaction per provider/model
route in the DeepSeek-harness shape. Defaults (`1.0`, `0.16`, `1`) preserve current behavior: the threshold ratio only
ever fires compaction _earlier_, the continuity tail is `min(20_000, window × retain_ratio)` (identical for windows ≥
125k, scaled down for small windows so the summary survives output bounding), and one halved retry happens on capacity
errors. Route overrides live in the `ROUTE_POLICIES` table matched on provider/model substrings; it is intentionally
empty until a route demonstrates a need.

## Verification

```bash
cargo nextest run --locked -p vtcode-core -E 'test(compaction::) | test(core::agent::state::) | test(core::agent::runner::tests::clearing::)'
cargo nextest run --locked -p vtcode -E 'test(llm_request::request_builder::tests::) | test(llm_request::context_management::tests::)'
./scripts/check-dev.sh
```

Regressions cover asymmetric 1/4/3 batches, reordered completions, trigger boundaries, provider/session intersections,
small-result non-growth, idempotence, durable history, and summary calls avoided or required at 32k/64k/96k/128k capacities.
Recording providers also check frozen summary prefixes, explicit output reserves, and impossible protocol minimums.
