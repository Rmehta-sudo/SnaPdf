#!/usr/bin/env python3
"""Synchronize, search, validate, and measure a project's durable memory."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import re
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Iterable


ROOT = Path(__file__).resolve().parents[2]
MEMORY = ROOT / "project-memory"
SESSIONS = Path.home() / ".codex/sessions"
BUILD = MEMORY / ".build"
MESSAGES = BUILD / "candidate_messages.jsonl"
COMMANDS = BUILD / "candidate_commands.jsonl"
COVERAGE = MEMORY / "audit/chat_coverage.tsv"
SOURCES = MEMORY / "sources/rollouts.tsv"
THREADS = MEMORY / "sources/threads.json"
EVIDENCE = MEMORY / "evidence/thread_messages.jsonl"
COMMAND_EVIDENCE = MEMORY / "evidence/command_evidence.jsonl"

VALID = {
    "preserved",
    "canonicalized",
    "superseded",
    "duplicate",
    "excluded_reconstructable",
    "excluded_chatter",
    "sensitive_omitted",
    "needs_review",
}

CHATTER = re.compile(
    r"(?i)^(ok|okay|yes|no|thanks|thank you|good|great|continue|proceed|do it|"
    r"what is taking so long|status|any update)[.!? ]*$"
)
PROGRESS = re.compile(r"(?i)^(i('|’)ll|i am|i’m|checking|working|i’m now|next i’ll)\b")
DURABLE = re.compile(
    r"(?i)(run[-_ ]?id|job[-_ ]?id|issue|ticket|pull request|commit|branch|tag|version|"
    r"config|baseline|ablation|hypothesis|decision|requirement|constraint|metric|accuracy|"
    r"loss|score|result|epoch|checkpoint|resume|error|exception|failed|root cause|fix|"
    r"dataset|database|schema|api|endpoint|entity|project|workspace|environment|container|"
    r"gpu|cuda|slurm|path|directory|cache|artifact|report|pdf|image|video|manifest|output|"
    r"/[A-Za-z0-9._-]+/|[A-Za-z]:\\)"
)
INTERNAL_SENTINELS = (
    "The following is the Codex agent history whose request action you are assessing",
    ">>> APPROVAL REQUEST START",
    "Reviewed Codex session id:",
)
INJECTED_BLOCKS = (
    re.compile(r"<recommended_plugins>.*?</recommended_plugins>", re.DOTALL),
    re.compile(r"<environment_context>.*?</environment_context>", re.DOTALL),
    re.compile(r"# AGENTS\.md instructions\s*<INSTRUCTIONS>.*?</INSTRUCTIONS>", re.DOTALL),
)
SECRET_PATTERNS = (
    re.compile(r"\bsk-[A-Za-z0-9_-]{16,}\b"),
    re.compile(r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}\b"),
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}\b"),
    re.compile(r"\bhf_[A-Za-z0-9]{20,}\b"),
    re.compile(
        r"(?i)\b((?:[A-Z0-9]+_)*(?:API_KEY|ACCESS_TOKEN|SECRET|PASSWORD))\b"
        r"(\s*=\s*)([^\s'\"]+)"
    ),
)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def redact(text: str) -> str:
    for pattern in SECRET_PATTERNS:
        if pattern.groups >= 3:
            text = pattern.sub(lambda m: f"{m.group(1)}{m.group(2)}<REDACTED>", text)
        else:
            text = pattern.sub("<REDACTED>", text)
    return text


def strip_injected(text: str) -> str:
    for pattern in INJECTED_BLOCKS:
        text = pattern.sub("", text)
    return text.strip()


def iter_jsonl(path: Path) -> Iterable[dict[str, Any]]:
    with path.open(encoding="utf-8", errors="replace") as handle:
        for line_number, line in enumerate(handle, 1):
            try:
                item = json.loads(line)
            except json.JSONDecodeError:
                continue
            item["_line"] = line_number
            yield item


def write_jsonl(path: Path, records: Iterable[dict[str, Any]]) -> int:
    path.parent.mkdir(parents=True, exist_ok=True)
    count = 0
    with path.open("w", encoding="utf-8") as handle:
        for record in records:
            handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
            count += 1
    return count


def session_meta(path: Path) -> tuple[str | None, Path | None, bool]:
    try:
        with path.open(encoding="utf-8", errors="replace") as handle:
            first = json.loads(next(handle))
    except (OSError, StopIteration, json.JSONDecodeError):
        return None, None, False
    if first.get("type") != "session_meta":
        return None, None, False
    payload = first.get("payload") or {}
    source = payload.get("source")
    subagent = source.get("subagent") if isinstance(source, dict) else None
    guardian = isinstance(subagent, dict) and subagent.get("other") == "guardian"
    internal = payload.get("thread_source") == "guardian_review" or guardian
    thread_id = payload.get("session_id") or payload.get("id")
    cwd = payload.get("cwd")
    return str(thread_id) if thread_id else None, Path(cwd) if cwd else None, internal


def belongs_to_root(cwd: Path | None) -> bool:
    if cwd is None:
        return False
    try:
        cwd.resolve().relative_to(ROOT.resolve())
        return True
    except (OSError, ValueError):
        return False


def configured_threads() -> dict[str, str]:
    if not THREADS.exists():
        return {}
    data = json.loads(THREADS.read_text(encoding="utf-8"))
    return {str(item["id"]): item.get("title") or str(item["id"]) for item in data.get("threads", [])}


def message_text(payload: dict[str, Any]) -> str:
    parts: list[str] = []
    for block in payload.get("content", []):
        if block.get("type") in {"input_text", "output_text"} and isinstance(block.get("text"), str):
            parts.append(block["text"])
    return redact(strip_injected("\n".join(parts)))


def command_text(value: Any) -> str:
    if isinstance(value, list):
        return " ".join(str(part) for part in value)
    if isinstance(value, str):
        return value
    return json.dumps(value, ensure_ascii=False, sort_keys=True)


def excerpt(text: str, limit: int = 12) -> str:
    lines = [redact(line.strip())[:500] for line in text.splitlines() if line.strip()]
    if len(lines) <= limit:
        return " | ".join(lines)
    return " | ".join(lines[:6] + ["…"] + lines[-5:])


def default_disposition(record: dict[str, Any]) -> tuple[str, str, str]:
    if record["kind"] == "command":
        return (
            "excluded_reconstructable",
            "project-memory/sources/rollouts.tsv",
            "Command interaction omitted; source hash and bounded audit preview retained.",
        )
    text = record["text"].strip()
    role = record["role"]
    normalized = " ".join(text.split())
    if CHATTER.fullmatch(normalized):
        return "excluded_chatter", "", "No durable project content."
    if role == "user":
        return (
            "preserved",
            "project-memory/evidence/thread_messages.jsonl",
            "Exact substantive user instruction or context retained conservatively.",
        )
    if record.get("phase") == "final":
        return (
            "preserved",
            "project-memory/evidence/thread_messages.jsonl",
            "Assistant final may contain non-reconstructable outcomes or handoff detail.",
        )
    if PROGRESS.search(text) and len(text) < 500 and not DURABLE.search(text):
        return "excluded_chatter", "", "Short progress narration without durable content."
    if DURABLE.search(text):
        return (
            "preserved",
            "project-memory/evidence/thread_messages.jsonl",
            "Potentially durable assistant evidence retained for targeted search.",
        )
    return (
        "excluded_reconstructable",
        "project Git history or source rollout",
        "Generic explanation or implementation detail expected to be reconstructable.",
    )


def export_candidates() -> tuple[list[dict[str, Any]], list[dict[str, Any]], list[dict[str, Any]]]:
    titles = configured_threads()
    files: list[tuple[str, str, Path]] = []
    if SESSIONS.exists():
        for path in sorted(SESSIONS.rglob("*.jsonl")):
            thread_id, cwd, internal = session_meta(path)
            if not thread_id or internal:
                continue
            if thread_id in titles or belongs_to_root(cwd):
                title = titles.setdefault(thread_id, f"Project task {thread_id}")
                files.append((thread_id, title, path))

    messages: dict[str, dict[str, Any]] = {}
    commands: dict[str, dict[str, Any]] = {}
    sources: list[dict[str, Any]] = []
    for thread_id, title, path in files:
        raw = path.read_bytes()
        sources.append(
            {
                "thread_id": thread_id,
                "title": title,
                "source_path": str(path),
                "bytes": len(raw),
                "sha256": sha256(raw),
            }
        )
        for item in iter_jsonl(path):
            payload = item.get("payload") or {}
            timestamp = item.get("timestamp", "")
            ordinal = item.get("ordinal")
            if item.get("type") == "response_item" and payload.get("type") == "message":
                role = payload.get("role")
                if role not in {"user", "assistant"}:
                    continue
                text = message_text(payload)
                if not text:
                    continue
                message_id = payload.get("id") or f"{path}:{item['_line']}:message"
                record_id = f"message:{message_id}"
                messages.setdefault(
                    record_id,
                    {
                        "record_id": record_id,
                        "kind": "message",
                        "thread_id": thread_id,
                        "thread_title": title,
                        "timestamp": timestamp,
                        "ordinal": ordinal,
                        "source_file": str(path),
                        "source_line": item["_line"],
                        "message_id": message_id,
                        "role": role,
                        "phase": payload.get("phase"),
                        "text": text,
                        "text_sha256": sha256(text.encode()),
                    },
                )
            elif item.get("type") == "event_msg" and payload.get("type") == "item_completed":
                event = payload.get("item") or {}
                if event.get("type") != "CommandExecution":
                    continue
                event_id = event.get("id") or f"{path}:{item['_line']}:command"
                record_id = f"command:{event_id}"
                output = (event.get("stdout") or "") + (event.get("stderr") or "")
                commands.setdefault(
                    record_id,
                    {
                        "record_id": record_id,
                        "kind": "command",
                        "thread_id": thread_id,
                        "thread_title": title,
                        "timestamp": timestamp,
                        "ordinal": ordinal,
                        "source_file": str(path),
                        "source_line": item["_line"],
                        "event_id": event_id,
                        "role": "",
                        "status": event.get("status"),
                        "exit_code": event.get("exit_code"),
                        "command": redact(command_text(event.get("command"))),
                        "output_bytes": len(output.encode(errors="replace")),
                        "output_sha256": sha256(output.encode(errors="replace")),
                        "evidence_excerpt": excerpt(output),
                    },
                )

    ordered_messages = sorted(messages.values(), key=lambda r: (r["timestamp"], r["record_id"]))
    ordered_commands = sorted(commands.values(), key=lambda r: (r["timestamp"], r["record_id"]))
    write_jsonl(MESSAGES, ordered_messages)
    write_jsonl(COMMANDS, ordered_commands)
    return ordered_messages, ordered_commands, sources


def load_previous_coverage() -> dict[str, dict[str, str]]:
    if not COVERAGE.exists() or COVERAGE.stat().st_size == 0:
        return {}
    with COVERAGE.open(encoding="utf-8", newline="") as handle:
        return {row["record_id"]: row for row in csv.DictReader(handle, delimiter="\t")}


def write_sources(sources: list[dict[str, Any]]) -> None:
    previous: dict[str, dict[str, Any]] = {}
    if SOURCES.exists() and SOURCES.stat().st_size:
        with SOURCES.open(encoding="utf-8", newline="") as handle:
            previous = {row["source_path"]: row for row in csv.DictReader(handle, delimiter="\t")}
    for row in sources:
        previous[row["source_path"]] = row
    SOURCES.parent.mkdir(parents=True, exist_ok=True)
    with SOURCES.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=("thread_id", "title", "source_path", "bytes", "sha256"),
            delimiter="\t",
            lineterminator="\n",
        )
        writer.writeheader()
        writer.writerows(sorted(previous.values(), key=lambda row: (row["title"], row["source_path"])))


def write_coverage(records: list[dict[str, Any]]) -> list[dict[str, str]]:
    old = load_previous_coverage()
    fields = (
        "record_id",
        "kind",
        "thread_id",
        "thread_title",
        "timestamp",
        "role_or_status",
        "preview",
        "disposition",
        "destination",
        "reason_or_fact_id",
        "source_file",
        "source_line",
    )
    rows: list[dict[str, str]] = []
    for record in records:
        prior = old.get(record["record_id"])
        disposition, destination, reason = default_disposition(record)
        if prior and prior.get("disposition") in VALID:
            disposition = prior["disposition"]
            destination = prior.get("destination", "")
            reason = prior.get("reason_or_fact_id", "")
        text = record.get("text") or record.get("command") or ""
        rows.append(
            {
                "record_id": str(record["record_id"]),
                "kind": str(record["kind"]),
                "thread_id": str(record["thread_id"]),
                "thread_title": str(record["thread_title"]),
                "timestamp": str(record["timestamp"]),
                "role_or_status": str(record.get("role") or record.get("status") or ""),
                "preview": " ".join(text.split())[:500],
                "disposition": disposition,
                "destination": destination,
                "reason_or_fact_id": reason,
                "source_file": str(record["source_file"]),
                "source_line": str(record["source_line"]),
            }
        )
    current_ids = {row["record_id"] for row in rows}
    rows.extend(row for record_id, row in old.items() if record_id not in current_ids)
    rows.sort(key=lambda row: (row["timestamp"], row["record_id"]))
    COVERAGE.parent.mkdir(parents=True, exist_ok=True)
    with COVERAGE.open("w", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=fields, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
    return rows


def publish(records: list[dict[str, Any]], rows: list[dict[str, str]]) -> tuple[int, int]:
    kept = {row["record_id"] for row in rows if row["disposition"] == "preserved"}
    combined: dict[str, dict[str, Any]] = {}
    for path in (EVIDENCE, COMMAND_EVIDENCE):
        if path.exists():
            for record in iter_jsonl(path):
                combined[record["record_id"]] = record
    for record in records:
        combined[record["record_id"]] = record
    messages = sorted(
        (
            record
            for record_id, record in combined.items()
            if record_id in kept and record.get("kind", "message") == "message"
        ),
        key=lambda record: (record.get("timestamp", ""), record["record_id"]),
    )
    commands = sorted(
        (
            record
            for record_id, record in combined.items()
            if record_id in kept and record.get("kind") == "command"
        ),
        key=lambda record: (record.get("timestamp", ""), record["record_id"]),
    )
    return write_jsonl(EVIDENCE, messages), write_jsonl(COMMAND_EVIDENCE, commands)


def validate(quiet: bool = False) -> list[str]:
    errors: list[str] = []
    required = (
        ROOT / "AGENTS.md",
        ROOT / "PROJECT_CONTEXT.md",
        MEMORY / "INDEX.md",
        MEMORY / "FACTS.md",
        MEMORY / "MAINTENANCE.md",
        EVIDENCE,
        COMMAND_EVIDENCE,
        SOURCES,
        COVERAGE,
    )
    for path in required:
        if not path.exists():
            errors.append(f"missing required path: {path.relative_to(ROOT)}")
    rows: list[dict[str, str]] = []
    if COVERAGE.exists() and COVERAGE.stat().st_size:
        with COVERAGE.open(encoding="utf-8", newline="") as handle:
            rows = list(csv.DictReader(handle, delimiter="\t"))
        ids = [row["record_id"] for row in rows]
        if len(ids) != len(set(ids)):
            errors.append("coverage contains duplicate record IDs")
        for row in rows:
            if row["disposition"] not in VALID:
                errors.append(f"{row['record_id']}: invalid disposition {row['disposition']!r}")
            if row["disposition"] == "needs_review":
                errors.append(f"{row['record_id']}: still needs review")
            if row["disposition"] in {"preserved", "canonicalized", "superseded", "duplicate"} and not row["destination"].strip():
                errors.append(f"{row['record_id']}: disposition requires destination")
    if EVIDENCE.exists():
        cold = EVIDENCE.read_text(encoding="utf-8", errors="replace")
        for sentinel in INTERNAL_SENTINELS:
            if sentinel in cold:
                errors.append(f"internal guardian/approval transcript reached cold evidence: {sentinel!r}")
    for path in MEMORY.rglob("*") if MEMORY.exists() else ():
        if not path.is_file() or ".build" in path.parts:
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for pattern in SECRET_PATTERNS[:-1]:
            if pattern.search(text):
                errors.append(f"possible secret in {path.relative_to(ROOT)}")
    if not quiet:
        if errors:
            print("Project-memory validation failed:")
            for error in errors[:100]:
                print(f"- {error}")
        else:
            print(f"Project-memory validation passed: {len(rows)} coverage records, 0 unresolved records.")
    return errors


def sync() -> None:
    messages, commands, sources = export_candidates()
    write_sources(sources)
    records = [*messages, *commands]
    rows = write_coverage(records)
    published_messages, published_commands = publish(records, rows)
    errors = validate()
    summary = {
        "threads": len({row["thread_id"] for row in sources}),
        "source_files": len(sources),
        "messages": len(messages),
        "commands": len(commands),
        "coverage": dict(sorted(Counter(row["disposition"] for row in rows).items())),
        "published_messages": published_messages,
        "published_commands": published_commands,
    }
    print(json.dumps(summary, indent=2))
    if errors:
        raise SystemExit(1)


def search(query: str, include_evidence: bool, limit: int) -> None:
    pattern = re.compile(query, re.IGNORECASE)
    found: list[str] = []
    compact = [ROOT / "PROJECT_CONTEXT.md", MEMORY / "FACTS.md", *sorted((MEMORY / "topics").glob("*.md"))]
    for path in compact:
        if not path.exists():
            continue
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
            if pattern.search(line):
                found.append(f"{path.relative_to(ROOT)}:{number}: {line.strip()}")
    if include_evidence and EVIDENCE.exists():
        for record in iter_jsonl(EVIDENCE):
            if pattern.search(record.get("text", "")):
                preview = " ".join(record["text"].split())[:500]
                found.append(
                    f"{record['record_id']} [{record['thread_title']} {record['timestamp']} {record['role']}]: {preview}"
                )
            if len(found) >= limit:
                break
    for item in found[:limit]:
        print(item)
    if not found:
        raise SystemExit(1)


def stats() -> None:
    source_rows: list[dict[str, str]] = []
    coverage_rows: list[dict[str, str]] = []
    if SOURCES.exists() and SOURCES.stat().st_size:
        with SOURCES.open(encoding="utf-8", newline="") as handle:
            source_rows = list(csv.DictReader(handle, delimiter="\t"))
    if COVERAGE.exists() and COVERAGE.stat().st_size:
        with COVERAGE.open(encoding="utf-8", newline="") as handle:
            coverage_rows = list(csv.DictReader(handle, delimiter="\t"))
    cold_chars = 0
    cold_records = 0
    if EVIDENCE.exists():
        for record in iter_jsonl(EVIDENCE):
            cold_records += 1
            cold_chars += len(record.get("text", ""))
    compact_paths = [ROOT / "AGENTS.md", ROOT / "PROJECT_CONTEXT.md", MEMORY / "INDEX.md"]
    canonical_paths = compact_paths + [MEMORY / "FACTS.md", MEMORY / "MAINTENANCE.md", *sorted((MEMORY / "topics").glob("*.md"))]
    result = {
        "read_first_estimated_tokens": sum(len(path.read_text(encoding="utf-8")) for path in compact_paths if path.exists()) // 4,
        "all_canonical_estimated_tokens": sum(len(path.read_text(encoding="utf-8")) for path in canonical_paths if path.exists()) // 4,
        "cold_records": cold_records,
        "cold_estimated_tokens": cold_chars // 4,
        "raw_source_files": len(source_rows),
        "raw_source_bytes": sum(int(row["bytes"]) for row in source_rows),
        "coverage": dict(sorted(Counter(row["disposition"] for row in coverage_rows).items())),
    }
    print(json.dumps(result, indent=2))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("sync")
    sub.add_parser("validate")
    sub.add_parser("stats")
    search_parser = sub.add_parser("search")
    search_parser.add_argument("query", help="case-insensitive regular expression")
    search_parser.add_argument("--evidence", action="store_true")
    search_parser.add_argument("--limit", type=int, default=80)
    args = parser.parse_args()
    if args.command == "sync":
        sync()
    elif args.command == "validate":
        if validate():
            raise SystemExit(1)
    elif args.command == "stats":
        stats()
    else:
        search(args.query, args.evidence, args.limit)


if __name__ == "__main__":
    main()
