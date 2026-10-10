scan for large and monolith files and module and plan deduplication and refactor and extract reusable components.

===

https://arxiv.org/pdf/2609.20804

===

fix Follow-up issue (pre-existing, separate): full-auto turn 1 now fails on OpenAI with 400 Tool choice 'task_tracker' not found in 'tools' parameter (reproduces on both gpt-6-luna and gpt-5.6-sol). The planner fix is correct and minimal; the turn-loop allowed_tools_auto vs serialized-tools mismatch needs its own investigation. I left it untouched to keep this change surgical.
