"""Summarize Harbor trials and flag answer-source references without rescoring."""

import argparse
import hashlib
import importlib.metadata
import json
import re
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
    hits, files, errors = [], [], []
    for path in sorted(agent_dir.rglob("*")):
        if not path.is_file() or path.suffix not in {".json", ".jsonl", ".txt"}:
            continue
        relative = str(path.relative_to(agent_dir))
        files.append(relative)
        text = path.read_text()
        if path.suffix == ".json":
            records = [(1, text)]
        else:
            records = enumerate(text.splitlines(), 1)
        for line, raw in records:
            if not raw.strip():
                continue
            try:
                values = strings(json.loads(raw)) if path.suffix != ".txt" else [raw]
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
        "status": "complete" if files and not errors else "incomplete",
        "matched": bool(hits) if files else None,
        "files_scanned": files,
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
    for job in jobs:
        paths = sorted(job.glob("*/result.json"))
        if not paths:
            raise ValueError(f"No trial results found in {job}")
        for path in paths:
            result = json.loads(path.read_text())
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jobs", nargs="+", type=Path)
    parser.add_argument("--oracle-job", action="append", default=[], type=Path)
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--rules", type=Path, default=Path(__file__).with_name("answer-source-rules.json"))
    args = parser.parse_args()
    rule_bytes = args.rules.read_bytes()
    rules = json.loads(rule_bytes)
    compiled = [(rule["id"], re.compile(rule["pattern"], re.IGNORECASE)) for rule in rules["rules"]]
    rows = trials(args.jobs, compiled)
    def total(field):
        values = [row[field] for row in rows]
        return sum(values) if all(value is not None for value in values) else None
    result = {
        "schema_version": 1,
        "date": datetime.now(timezone.utc).isoformat(),
        "dataset": args.dataset,
        "harbor_version": importlib.metadata.version("harbor"),
        "agent": rows[0]["agent"],
        "model": rows[0]["model"],
        "reasoning_effort": rows[0]["reasoning_effort"],
        "n_trials": len(rows),
        "score": sum(row["reward"] or 0 for row in rows) / len(rows),
        "score_denominator_includes_errors": True,
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
        "oracle_trials": trials(args.oracle_job),
    }
    print(json.dumps(result, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
