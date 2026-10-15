# Fresh verification before successful completion

Interactive and headless runs require fresh verification after successful code
mutations, including changes below the six-edit checkpoint. The checkpoint still
controls mutation pacing and retains its bounded repair allowance after failure.
Prose-only and dedicated planning writes, rejected edits, and known no-op diffs
retain their exemptions.
Exemptions check requested paths and the effective targets reported by file
operations, so prose/planning symlinks to code still require verification.
Planning exemptions require every target to be a planning artifact. Build and dependency inputs
such as `CMakeLists.txt` and `requirements.txt` require verification.

Verification evidence belongs to runtime state in the existing tool registry,
separate from source inspection. Each verifier has a launch identity, mutation
generation, session identity while running, and observed terminal verdict. A
check launched before a later edit cannot verify that edit. Active mutating
sessions must settle before completion; their settlement invalidates checks run
while they were active. Internal turns and prompt compaction retain runtime
verdicts. Restored history can establish debt, but never restores live ownership
of archived process handles or credits historical passing output as a new verdict.
Required checker verdicts and running session identities are protected from
optional-check eviction. The registry retains up to 32 optional checker entries
in addition to required entries, and reapplies that bound when requirements are
released. A newer attempt for the same command supersedes older evidence;
retention never makes a check fresh after another code edit.

Qualifying model-run checks can satisfy completion. Adopted tracker verification
commands remain mandatory; a different passing checker cannot replace them.
Requirements and observed checks match using the execution kernel's pipeline
normalization, while dispatch retains the original required command for policy
checks. Eligible declared custom checks count as verification rather than
invalidating their own verdict as mutations; command admission remains unchanged.
The interactive checkpoint shares this classification, including yielded custom
checks and the bounded repair allowance after failure.
Native terminal adapters may supply empty argv and environment containers.
Nonempty launch context still cannot receive command-only verification credit.
Otherwise the harness uses the existing configured verifier override and project
detection. Disabling tracker continuation does not disable changed-code
verification. An empty verification list never proves changed code verified.

A yielded command has no verdict. Headless verification resumes a matching owned
session from the current mutation generation before starting another command.
It follows that session through existing wait facilities, under the command
execution ceiling,
and checks cancellation between waits. Required commands run sequentially and
stop at the first unsuccessful result. Only observed terminal exit zero can
produce passing evidence. Nonzero exit is a failed check; denial, lost results,
timeout, and cancellation remain unverified. Unavailable sessions are cleaned up
through existing termination/release facilities. Output retained for verification
is bounded.

If no verifier is available or recovery budgets are exhausted, changes remain in
place and the run returns the existing blocked/failure outcome with unverified
status. Report the command, observed evidence, and blocker. A passing build
establishes a build result; behavioral correctness still needs an appropriate
regression or exercised user flow. Automatic coverage selection, browser checks,
and fingerprints for external workspace changes are outside this policy.

## Research provenance

The lifecycle distinction was checked against OpenAI Codex at commit
`d292f721242ef355ce5a38da90e0e33edc4790e6`,
[`codex-rs/core/tests/suite/unified_exec.rs`](https://github.com/openai/codex/blob/d292f721242ef355ce5a38da90e0e33edc4790e6/codex-rs/core/tests/suite/unified_exec.rs#L2380).
Its tests distinguish running session identities from terminal exits. This change
uses VT Code's existing execution facilities; no upstream implementation was
copied. Upstream is Apache-2.0, copyright 2025 OpenAI.
Selective retention was also compared with
[`process_manager.rs`](https://github.com/openai/codex/blob/d292f721242ef355ce5a38da90e0e33edc4790e6/codex-rs/core/src/unified_exec/process_manager.rs#L1750)
and its
[`process_manager_tests.rs`](https://github.com/openai/codex/blob/d292f721242ef355ce5a38da90e0e33edc4790e6/codex-rs/core/src/unified_exec/process_manager_tests.rs#L544).
Upstream protects recent process entries during bounded pruning. VT Code applies
selective retention to mandatory verifier evidence under its own completion
policy; no upstream implementation was copied.
[OpenAI's goals guidance](https://developers.openai.com/cookbook/examples/codex/using_goals_in_codex)
supports explicit completion evidence and blocked conditions. These sources inform
the policy and do not establish a measured VT Code reliability improvement.
