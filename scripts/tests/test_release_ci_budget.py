#!/usr/bin/env python3
"""Daily release budget boundaries and release entry-point wiring."""

import datetime
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("budget", ROOT / "scripts/check_release_ci_budget.py")
budget = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(budget)
TODAY = datetime.date(2026, 10, 18)


def run(run_id, status="completed", conclusion="success", created="2026-10-18T00:00:00Z"):
    return {"id": run_id, "status": status, "conclusion": conclusion, "created_at": created}


class ReleaseCiBudgetTests(unittest.TestCase):
    def test_all_pages_and_utc_boundary(self):
        pages = [
            {"workflow_runs": [run(11, created="2026-10-17T23:59:59Z"), run(22)]},
            {"workflow_runs": [run(33, "queued", None), run(44, "in_progress", None)]},
        ]
        self.assertEqual(budget.blocking_runs(pages, TODAY), [22, 33, 44])

    def test_failed_and_cancelled_runs_allow_retry(self):
        pages = [{"workflow_runs": [run(1, conclusion="failure"), run(2, conclusion="cancelled")]}]
        self.assertEqual(budget.blocking_runs(pages, TODAY), [])
        self.assertEqual(budget.blocking_runs([{"workflow_runs": []}], TODAY), [])

    def test_waiting_runs_block_and_timestamps_are_normalized(self):
        pages = [{"workflow_runs": [
            run(5, "waiting", None),
            run(6, created="2026-10-17T20:00:00-04:00"),
            run(7, created="2026-10-18T01:00:00+07:00"),
        ]}]
        self.assertEqual(budget.blocking_runs(pages, TODAY), [5, 6])

    def test_api_and_invalid_responses_fail_closed(self):
        for output in ("not-json", '[]', '[{}]', '[{"workflow_runs": null}]'):
            with self.subTest(output=output), patch.object(budget.subprocess, "run") as call:
                call.return_value.stdout = output
                self.assertEqual(budget.main(), 1)
        with patch.object(budget.subprocess, "run", side_effect=subprocess.TimeoutExpired("gh", 60)):
            self.assertEqual(budget.main(), 1)

    def test_cli_fetches_every_page_and_returns_policy_result(self):
        today = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
        for runs, expected in (([], 0), ([run(9, created=f"{today}T00:00:00Z")], 1)):
            with self.subTest(expected=expected), patch.object(budget.subprocess, "run") as call:
                call.return_value.stdout = json.dumps([{"workflow_runs": runs}])
                self.assertEqual(budget.main(), expected)
                args = call.call_args.args[0]
                self.assertEqual(args[:4], ["gh", "api", "--paginate", "--slurp"])
                self.assertIn(f"created={today}", args[4])
                self.assertIn("event=workflow_dispatch", args[4])

    def test_invalid_status_or_naive_timestamp_is_rejected(self):
        for entry in (run(1, "unknown"), run(2, created="2026-10-18T00:00:00"), run(3, conclusion=None)):
            with self.subTest(entry=entry), self.assertRaises(ValueError):
                budget.blocking_runs([{"workflow_runs": [entry]}], TODAY)

    def test_release_flags_and_guard_precede_side_effects(self):
        source = (ROOT / "scripts/release.sh").read_text()
        # Execute the actual parser/preflight with a rejecting budget command.
        # Stop at the first existing release operation; never run a release.
        prefix = source[source.index("main() {"):source.index("\n\tcheck_branch\n")]
        script = (
            'set -eu\nSCRIPT_DIR=/fixture\n'
            'print_info() { :; }\n'
            'python3() { printf "budget-check\\n"; return 1; }\n'
            + prefix + '\nprintf "proceed\\n"\n}\nmain "$@"\n'
        )
        for flags, blocked in (
            ([], True), (["--ci-only"], True),
            (["--ci-only", "--skip-binaries"], True),
            (["--ci-only", "--skip-release"], True),
            (["--force-ci"], False), (["--dry-run"], False),
            (["--skip-release"], False), (["--skip-binaries"], False),
        ):
            with self.subTest(flags=flags):
                result = subprocess.run(["bash", "-c", script, "fixture", *flags],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, int(blocked), result.stderr)
                self.assertEqual(result.stdout, "budget-check\n" if blocked else "proceed\n")


if __name__ == "__main__":
    unittest.main()
