# VT Code Companion private pilot

For installation, host support, examples, and troubleshooting, start with the
[user guide](../user-guide/vtcode-companion.md). The packager's `--marketplace`
option generates a fresh machine-specific local catalog using the same validated
components as ZIP export; it never installs or changes trust. The optional
`apps/webmcp/companion/` information page ships alongside the browser editor.
Public submission remains deferred.

## Local distribution milestone (2026-10-17)

The repository README and user guide now describe Companion as an experimental
local integration. The Vite build includes `companion/index.html` beside the
unchanged editor route, with the supplied branding banner and links to GitHub
documentation and Issues. No site deployment or registry submission was run.

Validation for this increment:

- Python packaging/marketplace suite: **13 passed**. New cases check catalog-relative
  plugin resolution, exact fixed launch arguments, paths containing spaces,
  preservation of an existing catalog, and rejection before writing invalid input.
- Ruff lint/format and scoped Markdown/whitespace checks passed.
- Browser TypeScript checking, **67 tests**, and production build passed. Vite
  reported its large editor chunk warning. Both built pages and their referenced
  local assets returned HTTP 200. Visual inspection remains unverified because
  this session had no available browser surface.
- Generated a fresh local marketplace and invoked its launcher directly: MCP
  initialization, all five tools, workspace README fetch, read-only planning,
  and EOF cleanup passed. Task `1cbace4c-2806-4b70-8293-532cd72075c9` completed
  with a 1,616-byte final response, `evidence_truncated=false`, no recorded
  changes, and `verification=unverified` (no verifier requested). The README
  content hash remained unchanged; the service exited zero on disconnect.
- This directly exercises the generated connection, not installation or tool
  attachment inside a fresh Codex/ChatGPT host. No installed cache, host settings,
  trust, or credentials were changed. No Rust source changed in this increment;
  Rust runtime suites were not rerun.

The local probe report is stored at
`target/vtcode-companion-local-setup-validation.json`. Existing implementation
evaluation remains unresolved. A later ChatGPT Desktop Local Work planning task
was reported successful; full review coverage and implementation remain unverified.

## GitHub sharing and installation paths

The repository includes a GitHub marketplace catalog and portable MCP
configuration. This repository source is the shareable package; recipients
install it from GitHub and configure their own VT Code binary and workspace.
Run `vtcode --workspace <path> companion setup --binary <path>`
to persist the exact executable and one canonical workspace in VT Code's
per-user config. The launcher reads those fixed paths; MCP arguments and host
working directory cannot override them. The config follows the platform's
existing VT Code config directory and is written using its private atomic-file
helper. Direct GitHub installation steps are in the
[user guide](../user-guide/vtcode-companion.md). Host-side GitHub installation
and plugin process startup still require fresh Codex and ChatGPT Desktop checks.
Codex marketplace names are unique; if a same-named marketplace points at an
older source or ref, remove that entry before adding the GitHub source. Do not
run the plugin-add command after a failed marketplace-add command, since Codex
may install from the already-registered source instead.
The `0.179.2` release tag includes the Companion source and CLI setup command,
but it predates the later merged task-failure fix. Use the repository's `main`
ref to test that fix privately. After publishing a VT Code release that contains
the fix, update installation instructions to pin that release tag. Keep the
plugin source and VT Code binary on compatible revisions.

Do not create a ZIP to share this local-only integration. The packager's local
`--local --marketplace` and `--local --output` modes generate per-user packages
containing the creator's absolute executable and workspace paths. Keep those
generated artifacts local and untracked. Share the GitHub repository
marketplace instead; each recipient must build or install a compatible VT Code
binary, then run `companion setup` for their own binary and workspace.

The repository's `scripts/install.sh` installs the latest published VT Code
release; it does not install the Companion plugin. Until a release includes
`webmcp stdio --companion` and `companion setup`, build the executable from the
same source checkout as the plugin package. `scripts/setup.sh` is a contributor environment script,
not a runtime installer. It updates Rust, installs development components and
nextest, and runs a locked Cargo check. Its test guidance consistently uses
nextest, as required by the repository testing workflow.

VT Code Companion exposes coding tasks to a local MCP client while execution
remains on the user's machine. The local plugin uses stdio, opens no listener,
and needs no tunnel, domain, or OAuth account. Public submission is deferred.
The optional HTTP transport remains available for a separately configured deployment.

## Local connection

Use a rebuilt VT Code binary. First configure the selected workspace through VT
Code, explicitly persist full-auto workspace trust, and enable
`automation.full_auto.enabled` with the intended tool allowlist. Companion never
grants trust or disables permission checks. Each child sets
`VTCODE_TRUST_WORKSPACE=deny`: an existing persisted trust record still works,
but environment and dotenv values cannot grant a new one.

The MCP client launches this command with stdin/stdout pipes:

```sh
/absolute/path/to/vtcode --workspace /absolute/path/to/workspace --quiet \
  webmcp stdio --companion
```

Stdout contains only MCP messages. Closing the input pipe ends the service and
cancels/reaps owned tasks. Without `--companion`, stdio exposes only `search` and
`fetch`. Stdio access belongs to the local process owner; coding tasks still use
the same persisted trust, allowlists, explicit denies, and exec sandbox checks.
Tool calls cannot select another workspace. It is intended for local desktop/CLI
hosts; web and mobile clients cannot launch this process.

Build the binary and create a machine-specific local ZIP:

```sh
cargo build --locked --bin vtcode
python3 scripts/package-vtcode-companion.py --local \
  --binary "$PWD/target/debug/vtcode" --workspace "$PWD" \
  --output "$PWD/target/vtcode-companion-local-0.1.1.zip"
```

The archive contains `plugin.json`, `mcp.json`, the workflow skill, a small
Python 3 launcher, and the referenced square icon and branding banner. Portable
manifests require a command name and a package-relative
working directory, so the launcher replaces itself with the exact VT Code binary
using `execv`. Its configuration records canonical absolute executable and
workspace paths as separate arguments, with no shell expansion. It contains no
provider credentials, proxy tokens, or binary.
Rebuild it for another machine/workspace; do not upload this local archive to the
public directory. The packager validates paths and archive contents; actual
runtime discovery must be tested separately. Import/install it using the local
host's supported plugin flow. Packaging does not install or enable the plugin.

### Branding update 0.1.1

The display name is **VT Code Companion**; `vtcode-companion` remains the package
identifier and `vtcode` denotes the CLI. Vinh Nguyen is the creator-selected
author, using the public `vinhnx` profile; Platform identity verification remains
separate. The creator confirmed free/open-source operation without plugin
payments and availability in all supported countries.

The logo and composer use an unchanged copy of
`resources/icons/vtcode-profile-512.png` (512×512, 29,619 bytes) in both appearances.
The gallery uses the supplied `resources/logo/vtcode_5.png` CLI branding banner
(1774×887, 9,508 bytes), not a runtime screenshot. The manifest includes refined
descriptions, three starter prompts, release notes, and five positive plus three
negative draft review cases. Legal URLs, a demo link, and verified organization
eligibility remain absent rather than fabricated.

The local artifact `target/vtcode-companion-local-0.1.1.zip` contains exactly six
files: the four runtime/skill components above plus `assets/icon.png` and
`assets/banner.png`. SHA-256:
`8158cf74c6187392cc9238a47c831a12f135b4748ed5e039ccb5ae910a92882b`.
Archive inspection checked PNG source-byte equality, dimensions, referenced
files, metadata/prompt limits, review case fields/counts, and credential markers.
The packaging suite passed 11 tests, including missing/escaping/symlinked assets
and nonsquare logo rejection. Ruff lint/format, Markdown lint, and scoped
whitespace checks passed. No Rust runtime changed or runtime suites were rerun
for this branding increment; previous runtime validation is recorded below.

The prepared local marketplace source was refreshed to 0.1.1. Reinstall/update
through the host's plugin flow and start a new session to load its new metadata;
the installed cache was not edited. This remains a private machine-specific ZIP,
not a public installer or a registry submission. See
[current submission readiness](vtcode-companion-submission.md#can-this-be-submitted-now).

## ChatGPT Desktop: tool attachment and partial planning evidence

Use a **local Work conversation** with VT Code Companion enabled. Loading the
skill or seeing a connected badge does not prove that the MCP tool definitions
reached that conversation. The workflow skill first uses host tool discovery,
when available, because tools can be deferred and names can carry server prefixes.
It must not diagnose a disabled server solely from missing model-visible tools.

1. In Settings > MCP servers, or `/mcp`, inspect the discovered tools. Expect
   `search`, `fetch`, `start_task`, `get_task`, and `cancel_task`.
2. If only `search` and `fetch` appear, check the launch command: it needs
   `webmcp stdio --companion`, with the fixed workspace configured by the host.
3. If all five appear but the model cannot call them, restart the MCP connection
   and start a new local Work conversation with Companion enabled. Ask it to
   discover the Companion task tools before reporting them unavailable. Capture
   the discovery result or exact connection error if that still fails.
4. Refresh/reinstall an updated plugin from its local marketplace source using
   the host's supported flow. Do not edit its installed cache. A skill update
   cannot repair a host that never exposes the connected MCP tools.

The [official MCP guide](https://learn.chatgpt.com/docs/extend/mcp) documents local
stdio setup and restarting connections. OpenAI's
[tool-search guide](https://developers.openai.com/api/docs/guides/tools-tool-search)
documents deferred MCP/plugin tools; that is a discovery possibility, not proof
of the failure's cause in a particular ChatGPT thread. Local stdio itself needs
no tunnel or public submission. A direct MCP protocol check verifies the launch
configuration, not model-side discovery inside ChatGPT Desktop. The official
shared-configuration guidance is scoped to the same Codex host; it must not be
treated as proof that a particular Work conversation received the tools.

An earlier follow-up Work response reported **neither Companion tools nor tool
search**, and fetched the public README through GitHub instead. That was not a
Companion call or access to the local dirty worktree. A later user-reported
ChatGPT Desktop Local Work task completed a Companion read-only plan for
`crates/codegen/vtcode-webmcp/src/remote_mcp.rs` and the loop detector changes:

- Task: `f9f84f19-349d-4eef-8c1d-1f5d91d98c4c`; no changes recorded.
- Reported inspection: `remote_mcp.rs`, loop detector, its runner caller and
  harness tests, WebMCP filesystem/library files, WebMCP manifest, `src/cli/webmcp.rs`
  wiring, plus working-tree status and Git history.
- Truncation: task metadata said evidence was not truncated. The report still
  described partial review coverage: some diff/test tails and
  `src/cli/companion.rs` were not fully inspected, and filesystem-helper callers
  were not enumerated.
- Verification: none; correctly reported as unverified for a read-only plan.

This confirms Companion tool attachment and one planning run in ChatGPT Desktop.
It does not establish a complete review, a passing test run, or implementation
reliability. Current status: Codex/local-protocol planning is verified; one
ChatGPT Desktop planning task is user-reported and partial; implementation and
the full cross-client workflow remain unverified. Desktop UI inspection from
this session remains unavailable, so host details rely on the user's report.

On 2026-10-17, a reported Local/connected ChatGPT thread could load the skill but
reported missing tools. The installed launch configuration independently passed
initialization, all five discovered tools, a harmless `README.md` fetch, and EOF
shutdown (exit zero). Desktop UI inspection was blocked, so the host-side cause
is unverified. The skill now attempts available tool discovery and avoids
inferring server failure from absent model-visible tools. Ten package tests,
Markdown lint, and scoped whitespace checks passed; no Rust code changed.

The updated private artifact is
`target/vtcode-companion-local-discovery-0.1.0.zip`, SHA-256
`f38ca65fddc58cef8ad571f2a965f9ab76e3c72791d8e970211ddb8f6635b13e`.
Its four components were reopened and checked, and the skill matches source.
The prepared local marketplace source was refreshed; the installed cache was
not edited and no plugin was installed or submitted by this diagnostic.
The follow-up provenance/host-blocker guidance is packaged in
`target/vtcode-companion-local-host-diagnostic-0.1.0.zip` (SHA-256
`c5a92e9ab3767b814e89587d1de09dc9276031042143ad76f78d0e01b89803dd`).
Its four components and exact skill contents were checked; the prepared
marketplace source was refreshed. This changes diagnostics only and does not
establish a working ChatGPT Work connection.

## Optional remote transport (deferred)

Configure the existing remote MCP transport and start it with an additional opt-in:

```sh
vtcode webmcp serve --mcp --companion \
  --allowed-root /absolute/path/to/workspace \
  --mcp-public-url https://YOUR-MCP-HOST/sse/ \
  --mcp-authorization-server https://YOUR-AUTHORIZATION-SERVER \
  --mcp-proxy-token-env VTCODE_WEBMCP_MCP_PROXY_TOKEN
```

The uppercase hosts are configuration placeholders, not deployed services.
Supply the internal proxy token through a secure environment. Never put it in
the command line, plugin manifest, repository, or task prompt. The listener binds
only to loopback. Omitting `--companion` preserves the two read-only tools.
Companion requires `--mcp` explicitly even if remote MCP is enabled in config.

## Task contract

Headless planning completes on a nonempty, tool-free terminal response, using
the normal planning completion gate. The canonical completion result retains
the model's plan instead of replacing it with the operational task summary.
Completion does not imply implementation approval or successful verification.

Shell loop detection compares normalized actions rather than treating every
`exec_command` use as the same action. Distinct inspection ranges can make
progress; identical reads, repeated action sequences, explicit operator limits,
navigation budgets, and harness turn/tool/wall-clock limits still apply. A hard
loop stop is recorded explicitly, rather than inferred from aggregate tool-name
usage when a soft warning is emitted.

| Tool | Input | Outcome |
| --- | --- | --- |
| `start_task` | `prompt`, optional `mode` (defaults to `plan`), optional `plan_task_id` | Starts one task and returns its opaque ID and running snapshot |
| `get_task` | `task_id` | Returns lifecycle, public response, recorded changes/diff, and verification |
| `cancel_task` | `task_id` | Requests cancellation; poll `get_task` until cleanup is terminal |

Planning runs `vtcode exec --json --dry-run`; implementation runs
`vtcode exec --json`. The binary fixes the executable, workspace, arguments, and
environment policy. Untrusted prompts are delivered through stdin and never parsed as arguments; tools
cannot override the launch. Child exec reuses the selected absolute session config path and the operator's
inline config, permission rules, security level, execution limits, and explicit
provider/model/agent choices through the existing CLI loader. The task caller
cannot supply these flags. Workspace config, sandboxing, explicit denies, and
canonical session persistence remain authoritative.

Implementation requires a completed, untruncated plan with a nonempty result
from the same service instance. Its prompt includes that plan and the new user
request. The packaged skill requires explicit user authorization; the plan ID
is only a workflow prerequisite, not cryptographic proof of user approval.
The workspace can change between tasks; implementation must inspect current
evidence, and existing edit conflict checks remain authoritative.

Only one task may run at a time. The service retains up to 64 task snapshots;
restart after reaching the cap. IDs do not survive restart. Canonical exec
history remains under the workspace's `.vtcode/sessions/`; planning may write
runtime session artifacts but uses read-only policy for coding tools.
Each task has a 30-minute deadline and a two-second post-exit output-drain limit. Prompt input is limited to 16 KiB; event
records are limited to 1 MiB. Public responses and diffs are each limited to
16 KiB, with at most 128 recorded changed paths. Truncation is explicit.

`completed` requires process exit zero and a successful canonical thread outcome.
Missing completion, malformed events, process failure, or unconfirmed cleanup
produce `failed`. No recorded verification means `unverified`, even after exit
zero. `verified` requires a runtime verification-passed event with a command and
exit zero. Later tool activity invalidates that proof conservatively. `failed`
verification is distinct from task failure. A failed or cancelled task retains
an observed failed check, discards positive proof, and marks its evidence incomplete. Changes derive from successful
canonical file-change events, not a whole-workspace Git diff; shell changes may
therefore lack attribution. Diffs without safe relative public paths are withheld.
Public text uses the shared secret sanitizer; raw stderr and private reasoning
are excluded. Sanitization is best effort, so only connect a trusted private client.

Post-setup execution errors must reach the runner's terminal failure recorder and
canonical session close. The execution body uses an awaited async block so `?`
cannot bypass that finalizer. Companion retains the first sanitized canonical
failure reason, falling back to the terminal stop reason, and includes process
exit or supervisor failure details in `progress`. Malformed event diagnostics
report the parse location without echoing the event payload. Cleanup and child
reap failures remain distinct; none of these diagnostics grant verification credit.

A live replay exposed an evaluator count/type mismatch: an array in
`high_severity_findings` failed deserialization into the integer count. The shared
single-evaluator/skeptic prompt now states the count and array field types
explicitly. Parsing remains strict: arrays are not coerced into counts, and a
nonzero high-severity count still rejects a pass verdict.

Tracker updates to different indices or states use normalized action identities
in the generic headless loop guard. They do not consume one aggregate default
tracker repetition allowance. Identical polling and updates remain bounded, and
explicit operator-configured tool limits retain their aggregate meaning.

Evaluator requests include up to 12 recent retained tool results, paired only
with preceding matching function calls. The sanitized excerpt is bounded to
16 KiB and excludes assistant reasoning, invocation-only claims, and records
before the retained current-task input. If that boundary is missing, the
evaluator receives an explicit evidence gap. Missing or
cleared records are not proof of nonexecution; these excerpts do not replace
required final verification. Planner and replanner guidance requires executable
final-state tracker assertions that remain satisfiable after cleanup, rather
than implementation writes/deletes or prose falsifiers in `verify` fields.

Evaluator responses have a 4096-token allowance. JSON-only sub-role requests
permit one tool-free concise rewrite when the provider reports output-length
truncation; a second truncated response fails explicitly. Other parse errors
retain finish-reason and output-budget context. The implementation is not
replayed by this response repair, and malformed verdicts remain rejected.

The failure-reason comparison used OpenAI Codex's
[`codex-rs/exec/src/event_processor_with_jsonl_output.rs`](https://github.com/openai/codex/blob/ce0323f2f2d3b25e82806cd555237da3db4e1897/codex-rs/exec/src/event_processor_with_jsonl_output.rs#L539)
at commit `ce0323f2f2d3b25e82806cd555237da3db4e1897`
(Apache-2.0, copyright 2025 OpenAI). VT Code reuses its own `ThreadEvent` contract
and finalizer; no upstream implementation was copied.

Cancellation requests a process-group interrupt, allows a short cleanup window,
then terminates remaining owned processes and reaps the child. Cancellation is
not rollback. Listener shutdown cancels and joins task supervisors before return.
The transport crate owns only task interfaces; launch/supervision belongs to the binary.

### Local validation (2026-10-17)

The installed local plugin's stdio launcher completed MCP initialization, tool
discovery, search/fetch, planning, and approved implementation using the rebuilt
debug binary. Plan `f748cd96-bd39-4ae2-89c0-e7263e875fcd` and implementation
`74cee6f9-3782-41da-b6de-0e7a47ef7856` completed with
`evidence_truncated: false`. Canonical tool events recorded exit zero for exact
`cmp` against the literal smoke-test text, removal, and the final file/symlink
absence assertion. The snapshot's separate mandatory verifier was
`cargo check --locked`, exit zero. The canonical manifest closed with
`in_turn: false`; service shutdown exited zero and the tracked diff was unchanged.
An earlier failed replay generated `memories/progress.md`; that extra artifact
was removed after validation.

The focused runner, evaluator, loop-detector, Companion, prompt, stdio, and inline
event suites passed 456 tests. The development gate, locked binary build, scoped
Markdown lint, and diff whitespace check passed. This validates the installed
launcher directly; an already-running Codex connection must restart to load the
rebuilt executable.

## Authentication and connection

The existing contract is documented in [WebMCP development](webmcp.md#remote-mcp-transport).
VT Code validates a private internal bearer token. An external proxy must:

1. Terminate HTTPS and validate the user's OAuth token, issuer, audience/resource,
   expiration, and applicable permissions.
2. Strip incoming authorization and any caller-supplied proxy identity headers.
3. Inject the configured internal bearer token before forwarding to loopback.
4. Preserve MCP HTTP methods and content negotiation, and publish the protected
   resource metadata route as JSON without a login-page redirect.

VT Code advertises an authorization server; it does not implement OAuth login,
token issuance, JWKS, or a tunnel. Provider/model credentials used by VT Code are
separate from companion-client authentication. See the
[official plugin authentication guide](https://developers.openai.com/plugins/build/auth).
A reachable tunnel alone does not satisfy authentication.

First test with a local MCP client and an internal bearer supplied through that
client's secure configuration. Then configure a stable authenticated HTTPS
tunnel, test OAuth discovery/login, and repeat the workflow from ChatGPT.
Client installation and account configuration are explicit setup steps; building
the source does not install a plugin in Codex or ChatGPT.

Connection acceptance: unauthenticated calls return 401; authenticated
`initialize` works; `tools/list` advertises all five tools; `search`/`fetch` remain
bounded; planning changes no source files; explicitly requested implementation
returns recorded evidence; cancellation reaches a terminal state. Repeat from
both clients before calling the private pilot connected.

## Remote packaging (deferred)

The source is in `plugins/vtcode-companion`. The local packager generates its
stdio `mcp.json` from explicit machine paths. Remote packaging instead requires
an actual endpoint to be verified. Set a short-lived external OAuth
access token in `VTCODE_COMPANION_ACCESS_TOKEN` through your secure environment,
then run:

```sh
python3 scripts/package-vtcode-companion.py \
  --mcp-url https://YOUR-MCP-HOST/mcp \
  --output /absolute/path/outside/plugin/vtcode-companion-0.1.0.zip
```

The packager checks public resource metadata, rejected unauthenticated access,
authenticated initialization, all five tools, and conservative action hints.
It writes the verified OAuth resource into portable `mcp.json`, excludes tokens,
and reopens the ZIP to inspect its exact contents. Existing artifacts are never
overwritten. These checks prove discovery, not model-provider execution or login
from both clients. Do not represent a source-only package or fixture test as a
connected plugin. Upload, installation, submission, and publication are separate.
Public review preparation, draft acceptance cases, and demo recording steps are
tracked in [submission preparation](vtcode-companion-submission.md).

## Research and verification

DeepWiki discovery returned client-side MCP paths rather than a comparable task
server. Direct inspection pinned to OpenAI Codex commit
`e16b631d08cdf82773831f9bbfb793da687d5299` verified conservative approval behavior in
[`codex-rs/core/src/mcp_tool_call_tests.rs`](https://github.com/openai/codex/blob/e16b631d08cdf82773831f9bbfb793da687d5299/codex-rs/core/src/mcp_tool_call_tests.rs):
destructive tools require approval even if marked read-only. The companion marks
start and cancellation as non-read-only, destructive actions. The same pinned source's
[`codex-rs/codex-mcp/src/binding.rs`](https://github.com/openai/codex/blob/e16b631d08cdf82773831f9bbfb793da687d5299/codex-rs/codex-mcp/src/binding.rs)
retains immutable permission authority in `PreparedMcpCall`. VT Code preserves
operator-selected child launch policy using its existing CLI and session-config
loader rather than copying that implementation. No upstream code was copied.
OpenAI Codex is Apache-2.0, copyright 2025 OpenAI. No verified upstream
task-server implementation is claimed; execution ownership follows local VT Code code.

Focused checks:

```sh
cargo nextest run --locked -p vtcode -p vtcode-webmcp \
  -E 'test(cli::companion) | test(remote_mcp)'
python3 -m unittest discover -s scripts/tests -p 'test_package_vtcode_companion.py'
cargo clippy --locked -p vtcode -p vtcode-core -p vtcode-webmcp --all-targets -- -D warnings
cargo fmt --all -- --check
./scripts/check-dev.sh
```

Process fixtures exercise real child supervision without a paid provider.
Live provider, Codex, ChatGPT, tunnel, and OAuth checks require the actual configured
services and must be reported separately. macOS process tests do not establish
Linux or Windows behavior.

For local stdio, `tests/companion_stdio.rs` starts the real binary with isolated
configuration and checks discovery, exact fixture reads, sensitive/path-escape
rejection, denied trust escalation, and EOF shutdown without a provider request.
The handler uses the same tool router for local and HTTP transports. DeepWiki
again returned client-side launch code rather than server startup; pinned Codex
`e16b631d08cdf82773831f9bbfb793da687d5299`,
`codex-rs/rmcp-client/tests/process_group_cleanup.rs`, directly verifies explicit
shutdown with in-flight work and process-group cleanup. No source was copied.
The server-side stdio path follows the installed rmcp transport API; no upstream
server implementation is claimed.

## Current local validation

On 2026-10-17, the affected locked nextest selection passed **87 tests**, with
zero failures and 8233 tests outside the selection. It included all WebMCP crate
tests and the new real-process stdio tests. The Python package suite passed
**10 tests**. The local archive validated as one skill and one MCP server.
The final stdio-only rerun passed both tests after test-helper cleanup. Affected
strict Clippy (`--all-targets -- -D warnings`), Rust formatting, Ruff lint/format,
Markdown lint, scoped diff checks, and `check-dev.sh --quiet` passed. The development
gate completed in 1m 20s and did not run tests. The broader core test build emitted
a nonfatal macOS unwind-table size linker warning; its test run exited zero.

An independent smoke test extracted the ZIP and launched its actual Python
entrypoint. It verified initialization, all five tools, an actual README fetch,
rejection of implementation without a plan, overlap rejection, task cancellation,
and EOF shutdown with exit zero. The temporary extraction was removed. This is
direct protocol validation, not proof of installation or invocation inside Codex.
No plugin was installed, uploaded, or submitted.

Earlier live-provider runs did change a disposable fixture and pass its check,
but failed at harness evaluator-response parsing; the task correctly returned
failed/unverified. That error remains unresolved. These new local transport and
packaging checks do not establish successful model-driven implementation. See
the retained [validation evidence](vtcode-companion-submission.md#observed-validation-on-2026-10-17).

The local ZIP contains exactly four files:

```text
plugin.json
mcp.json
skills/vtcode-companion/SKILL.md
server/launch.py
```

Its SHA-256 is
`74e02b2e59b15ea63f5070963e5292f012e049141ab065d65dc4993b46e83136`.

## Planning loop regression (2026-10-17)

Codex session `01a148af-11e8-7900-a4cd-fe45c3902a4e` invoked the installed local
Companion twice. Tasks `df3445e9-14ff-488c-b2fa-cb5a50a9327d` and
`e2385be1-4ea6-4045-b671-3eb6ce6ea24c` map to canonical VT Code sessions
`session-vtcode-20261017T070732Z_815633-75293` and
`session-vtcode-20261017T070930Z_965152-75910`. Both ended with `loop_detected`.
Their event histories show different shell inspections being classified as the
oscillation `[exec_command -> exec_command]`, followed by a hard stop inferred
from aggregate tool-name counts. `evidence_truncated` marked incomplete failed
execution; it was not evidence that a completed plan exceeded the output cap.

The repair counts normalized command actions, records explicit hard stops, and
keeps custom operator limits aggregate. A terminal, nonempty, tool-free planning
response enters the existing completion assessment without needing prose
completion keywords. Successful planning emits the actual plan as the final
agent message and canonical completion result. Permission and verification
gates remain in place; failed and cancelled tasks still report incomplete evidence.

A later read-only Companion replay exposed another false positive: fuzzy
oscillation detection reduced every repeated `code_search` call to the same tool
name, even when each call searched for different evidence. Fuzzy oscillation now
requires at least two distinct tool names in the repeated pattern. Exact
normalized-action repetition, navigation budgets, and alternating multi-tool
pattern detection remain active.

The last configured planning turn is reserved for tool-free synthesis, so lengthy
inspection cannot consume every turn before attempting an answer. The model must
state evidence gaps. Native or textual tool calls on that final turn are rejected
without execution and retain a failed turn-limit outcome; no extra turn is granted.
Provider output-token truncation also triggers one tool-free rewrite into a shorter
complete plan, using the same output cap and remaining turn budget. Completion
keywords in a truncated draft cannot count as success. If the rewrite is still
truncated, the task fails explicitly instead of repeating the draft through idle recovery.
Headless planning uses a 4096-token fallback output allowance, matching the existing
skill-synthesis allowance and compaction output reserve, rather than the generic
2000-token task default. Explicit provider sampling limits keep precedence. Replay
exposed the smaller default truncating both the initial draft and its rewrite.

DeepWiki discovery was checked against pinned OpenAI Codex commit
`e16b631d08cdf82773831f9bbfb793da687d5299`,
[`codex-rs/core/src/stream_events_utils.rs`](https://github.com/openai/codex/blob/e16b631d08cdf82773831f9bbfb793da687d5299/codex-rs/core/src/stream_events_utils.rs)
and its
[`stream_events_utils_tests.rs`](https://github.com/openai/codex/blob/e16b631d08cdf82773831f9bbfb793da687d5299/codex-rs/core/src/stream_events_utils_tests.rs):
visible assistant text in a final or unphased message is treated as terminal,
while nonterminal text does not close the turn. This supports terminal-phase
completion rather than keyword matching; it does not establish VT Code's loop
thresholds. No upstream code was copied (Apache-2.0, copyright 2025 OpenAI).

After rebuilding `target/debug/vtcode`, restart Codex to replace an already
running MCP process. The installed local launcher points directly to that binary;
these runtime fixes do not require changes to the plugin ZIP or cached manifest.
Nextest builds test executables and must not substitute for
`cargo build --locked --bin vtcode` when validating this launch path.

### Observed regression validation

The final locked nextest selection passed **300 tests**, zero failed, 8026 outside
the selection. It covered the loop detector, all headless-runner tests, Companion,
remote/local MCP routing, planning prompt presence/size, skill execution, and the
autonomous executor. New regressions cover 18 distinct real shell inspections
followed by a plain plan, actual repeated-action hard stops, explicit operator
limits, final-turn tool rejection (native and supported textual forms), blank
answers, and one bounded truncated-plan rewrite. A second truncated draft fails
without accepting completion keywords. Affected strict Clippy passed with
`--all-targets -- -D warnings`; the package suite passed 10 Python tests. The
macOS test linker emitted its nonfatal unwind-table size warning.
Final Rust formatting, Markdown lint, and scoped whitespace checks passed.
`./scripts/check-dev.sh --quiet` passed in 36 seconds (formatting, default-member
Clippy/compilation, and shell lint; tests were run separately with nextest).

After an explicit binary rebuild, the exact first archived Codex request was
replayed through the **installed** `vtcode-local/vtcode-companion/0.1.0` Python
launcher over MCP stdio. Canonical session
`session-vtcode-20261017T074302Z_404837-23278` returned `success` after 20 completed
tool calls. `get_task` returned `completed`, the actual **9699-byte** plan,
`evidence_truncated=false`, no attributed changes, and `verification=unverified`
(correct for planning without implementation/checks). The tracked source diff
remained unchanged, excluding the private TODO path from inspection; EOF shutdown
exited zero. This verifies the live provider and installed launch configuration
at protocol level. Restarting the user's existing Codex MCP process and invoking
it again from the host remain user-side steps; ChatGPT and remote transport were
not exercised.
