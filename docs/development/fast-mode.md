# Fast mode

`/fast [on|off]` controls the existing `provider.openai.service_tier` setting. Bare `/fast` toggles ultrafast;
`on` enables it and `off` removes the override, restoring the OpenAI project default. Turning it off does not
restore an earlier flex or priority choice. Arguments are case-insensitive; other or extra arguments show usage
without changing settings.

Only native OpenAI API-key routes for `gpt-6-astra` and `gpt-6.1-sol` support enabling ultrafast. Other OpenAI models,
ChatGPT subscription routes, gateways, and custom providers reject enabling. Choose API-key authentication and a
supported model with `/model`. Disabling remains available after a model or authentication switch.
Ultrafast costs more and has separate rate limits; see the
[official OpenAI guide](https://developers.openai.com/api/docs/guides/ultrafast-mode).

The TUI offers one informational reminder per session when an eligible model is selected with an OpenAI API key
and ultrafast is not enabled. It names `/fast on` and mentions the higher cost. The reminder reevaluates eligibility
at idle after model/auth changes, without credential discovery or network calls. It stays hidden for subscription,
gateway, custom-provider, missing-key, and noninteractive routes. Enabling ultrafast or successfully running `/fast`
also dismisses the reminder for the rest of the session, including after `/fast off`.

Changes apply to subsequent requests and persist using the model picker's `ConfigManager` path. Repeated explicit
settings are idempotent in their resulting configuration. Each accepted command rebuilds the client and persists the
choice, reconciling defaults captured before a configuration reload and runtime/disk differences. The handler prepares
a replacement provider client with the new tier and the existing context-window wrapper before saving. It installs
that client and runtime configuration only after saving succeeds.
Initialization or save failures leave active settings and the client untouched. Only the tier field changes; model,
reasoning effort, authentication, conversation history, and WebSocket configuration are preserved.

The header reflects the configured tier immediately. When enabled, ultrafast appears as a padded `ULTRAFAST` badge
with a premium `#D99A4E` gold fill and black text in both header layouts, exceeding WCAG AAA text contrast (7:1).
The badge stays undimmed, respects bold-is-bright, uses ANSI black/yellow in safe-color mode, and falls back to a
monochrome inverse label when colors are disabled. Other tiers retain their normal labels.
Rebuilding the provider permits its WebSocket connection to reconnect normally. Both HTTP request builders and
the shared WebSocket payload path apply model compatibility to
explicit tiers and provider defaults. The existing server-rejection fallback remains: configuration success confirms
the requested tier, while server-reported telemetry identifies the tier actually served.

## Implementation reference

The active-context plus persistent-selection pattern was verified against
[`codex-rs/tui/src/chatwidget/service_tiers.rs` at `bb64b894fd67158be0f874872e6d7bfe717e7368`](https://github.com/openai/codex/blob/bb64b894fd67158be0f874872e6d7bfe717e7368/codex-rs/tui/src/chatwidget/service_tiers.rs),
Apache-2.0, copyright 2025 OpenAI. VT Code uses its existing factory and configuration manager; no upstream code was
copied. VT Code additionally prepares the client before saving and installs it after saving for atomic active state.

## Verification

Focused coverage includes command discovery and parsing, compatibility and picker availability, tier transitions,
idempotence, runtime/disk drift, clearing a stale client after reload, subscription-route rejection, clearing after a
model switch, preservation of unrelated configuration, initialization/save failure
atomicity, and HTTP/WebSocket payloads with explicit and inherited tiers. Header tests cover both layouts, narrow
terminals, theme contrast, and color/bold accessibility policies. Run:

```sh
cargo nextest run --locked -p vtcode -p vtcode-config -p vtcode-skills -p vtcode-llm --no-tests=fail \
  -E 'test(fast) | test(service_tier) | test(available_service_tiers)'
cargo nextest run --locked -p vtcode-ui --no-tests=fail -E 'test(header) | test(theme) | test(ultrafast_badge)'
./scripts/check-dev.sh
```

These fixtures do not make live billable OpenAI calls or establish account-specific access, latency, or served tiers.
