"""Summarize Harbor trials and flag answer-source references without rescoring."""

import argparse
import hashlib
import importlib.metadata
import json
import re
import tomllib
from datetime import datetime, timezone
from pathlib import Path


def strings(value):
    if isinstance(value, str):
        yield value.replace("\\/", "/")
    elif isinstance(value, dict):
        for child in value.values():
            yield from strings(child)
    elif isinstance(value, list):
        for child in value:
            yield from strings(child)


def audit(agent_dir, rules):
    hits, files, errors, skipped = [], [], [], []
    for path in sorted(agent_dir.rglob("*")):
        relative = str(path.relative_to(agent_dir))
        # Do not follow log symlinks outside this evidence tree.
        if path.is_symlink():
            skipped.append({"file": relative, "reason": "symlink"})
            continue
        if path.is_dir():
            continue
        try:
            text = path.read_text(encoding="utf-8")
            if "\x00" in text:
                raise ValueError("binary file")
        except (OSError, UnicodeError, ValueError) as exc:
            skipped.append({"file": relative, "reason": type(exc).__name__})
            continue
        files.append(relative)
        if path.suffix == ".json":
            records = [(1, text)]
        else:
            records = enumerate(text.splitlines(), 1)
        for line, raw in records:
            if not raw.strip():
                continue
            try:
                values = strings(json.loads(raw)) if path.suffix in {".json", ".jsonl", ".cast"} else [raw]
            except json.JSONDecodeError:
                errors.append({"file": relative, "line": line})
                values = [raw]
            for value in values:
                for rule, pattern in rules:
                    for match in pattern.finditer(value):
                        hit = {
                            "rule": rule, "file": relative,
                            "line": line, "match": match.group(),
                        }
                        if hit not in hits:
                            hits.append(hit)
    return {
        "status": "complete" if files and not errors and not skipped else "incomplete",
        "matched": bool(hits) if files else None,
        "files_scanned": files,
        "files_not_scanned": skipped,
        "parse_errors": errors,
        "hits": hits,
        "score_changed": False,
    }


def seconds(timing):
    if not timing or not timing.get("started_at") or not timing.get("finished_at"):
        return None
    return (datetime.fromisoformat(timing["finished_at"])
            - datetime.fromisoformat(timing["started_at"])).total_seconds()


def trials(jobs, rules=None):
    rows = []
    seen = set()
    for job in jobs:
        if not job.is_dir():
            raise ValueError(f"Job directory does not exist: {job}")
        paths = sorted(job.glob("*/result.json"))
        for path in paths:
            result = json.loads(path.read_text())
            name = result["task_name"]
            if name in seen:
                raise ValueError(f"Duplicate task result: {name}")
            seen.add(name)
            usage = result.get("agent_result") or {}
            rewards = (result.get("verifier_result") or {}).get("rewards") or {}
            reward = rewards.get("reward")
            error = result.get("exception_info")
            row = {
                "task": result["task_name"], "trial": result["trial_name"],
                "job": job.name, "task_checksum": result["task_checksum"],
                "agent": result["agent_info"],
                "model": result["config"]["agent"].get("model_name"),
                "reasoning_effort": result["config"]["agent"]["kwargs"].get("reasoning_effort"),
                "started_at": result["started_at"], "finished_at": result["finished_at"],
                "reward": reward, "passed": reward == 1 if reward is not None else None,
                "exception_type": error["exception_type"] if error else None,
                "total_seconds": seconds(result),
                "agent_seconds": seconds(result.get("agent_execution")),
                "input_tokens": usage.get("n_input_tokens"),
                "cached_input_tokens": usage.get("n_cache_tokens"),
                "output_tokens": usage.get("n_output_tokens"),
                "cost_usd": usage.get("cost_usd"),
                "metadata": usage.get("metadata"),
            }
            if rules is not None:
                row["answer_source_check"] = audit(path.parent / "agent", rules)
            rows.append(row)
    return rows


def expected_tasks(directory):
    names = []
    for path in sorted(directory.glob("*/task.toml")):
        name = tomllib.loads(path.read_text())["task"]["name"]
        if name in names:
            raise ValueError(f"Duplicate task definition: {name}")
        names.append(name)
    if not names:
        raise ValueError(f"No task definitions found in {directory}")
    return names


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jobs", nargs="+", type=Path)
    parser.add_argument("--oracle-job", action="append", default=[], type=Path)
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--tasks-dir", required=True, type=Path,
                        help="Exported tasks expected in this run (a subset directory for a pilot)")
    parser.add_argument("--rules", type=Path, default=Path(__file__).with_name("answer-source-rules.json"))
    args = parser.parse_args()
    rule_bytes = args.rules.read_bytes()
    rules = json.loads(rule_bytes)
    compiled = [(rule["id"], re.compile(rule["pattern"], re.IGNORECASE)) for rule in rules["rules"]]
    rows = trials(args.jobs, compiled)
    expected = expected_tasks(args.tasks_dir)
    actual = {row["task"] for row in rows}
    if extra := actual - set(expected):
        raise ValueError(f"Unexpected tasks: {sorted(extra)}")
    missing = sorted(set(expected) - actual)
    oracle_rows = trials(args.oracle_job)
    oracle_by_task = {row["task"]: row for row in oracle_rows}
    if args.oracle_job:
        for row in rows:
            oracle = oracle_by_task.get(row["task"])
            checksum = oracle["task_checksum"] if oracle else None
            row["oracle_checksum_check"] = {
                "status": ("missing" if not checksum or not row["task_checksum"] else
                           "match" if checksum == row["task_checksum"] else "mismatch"),
                "oracle_task_checksum": checksum,
            }
    def total(field):
        values = [row[field] for row in rows]
        return sum(values) if not missing and all(value is not None for value in values) else None
    first = rows[0] if rows else {}
    result = {
        "schema_version": 2,
        "date": datetime.now(timezone.utc).isoformat(),
        "dataset": args.dataset,
        "harbor_version": importlib.metadata.version("harbor"),
        "agent": first.get("agent"),
        "model": first.get("model"),
        "reasoning_effort": first.get("reasoning_effort"),
        "n_trials": len(rows),
        "n_expected_tasks": len(expected),
        "expected_tasks": expected,
        "missing_tasks": missing,
        "score": sum(row["reward"] or 0 for row in rows) / len(expected),
        "score_denominator_includes_errors": True,
        "score_denominator_includes_missing": True,
        "totals": {field: total(field) for field in (
            "total_seconds", "agent_seconds", "input_tokens",
            "cached_input_tokens", "output_tokens", "cost_usd",
        )},
        "answer_source_rules": {
            "version": rules["version"],
            "sha256": hashlib.sha256(rule_bytes).hexdigest(),
            "interpretation": "References require human review; no score is changed. No match is not proof of no answer access.",
        },
        "trials": rows,
        "oracle_trials": oracle_rows,
    }
    print(json.dumps(result, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
