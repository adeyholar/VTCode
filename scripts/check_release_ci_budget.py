#!/usr/bin/env python3
"""Check the UTC daily release-build budget before release side effects."""

import datetime
import json
import subprocess
import sys


def blocking_runs(pages, today):
    """Failed/cancelled builds do not consume the daily successful-build budget."""
    if not isinstance(pages, list) or not pages:
        raise ValueError("missing paginated workflow response")
    active_statuses = {"queued", "in_progress", "waiting", "pending", "requested"}
    blockers = []
    for page in pages:
        if not isinstance(page["workflow_runs"], list):
            raise ValueError("invalid workflow run list")
        for run in page["workflow_runs"]:
            created = datetime.datetime.fromisoformat(run["created_at"].replace("Z", "+00:00"))
            if created.tzinfo is None:
                raise ValueError("run timestamp has no timezone")
            if created.astimezone(datetime.timezone.utc).date() != today:
                continue
            status = run["status"]
            if status not in active_statuses | {"completed"}:
                raise ValueError("unknown workflow status")
            if status == "completed" and run["conclusion"] not in {
                "success", "failure", "neutral", "cancelled", "skipped",
                "timed_out", "action_required", "stale", "startup_failure",
            }:
                raise ValueError("unknown completed workflow conclusion")
            if status in active_statuses or run["conclusion"] == "success":
                blockers.append(run["id"])
    return blockers


def main():
    today = datetime.datetime.now(datetime.timezone.utc).date()
    endpoint = (
        "repos/vinhnx/VTCode/actions/workflows/build-linux-windows.yml/runs"
        f"?event=workflow_dispatch&created={today.isoformat()}&per_page=100"
    )
    try:
        result = subprocess.run(
            ["gh", "api", "--paginate", "--slurp", endpoint],
            check=True, capture_output=True, text=True, timeout=60,
        )
        blockers = blocking_runs(json.loads(result.stdout), today)
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        print("Cannot verify the daily release CI budget; no release should start. "
              "Retry the check or use --force-ci for an urgent release.", file=sys.stderr)
        return 1
    if blockers:
        print(
            f"Release CI already succeeded or is active on {today} (UTC), "
            f"run(s): {', '.join(str(run_id) for run_id in blockers)}. "
            "Batch remaining changes for the next UTC day, or use --force-ci "
            "for an urgent release. No version or tag has been changed.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
