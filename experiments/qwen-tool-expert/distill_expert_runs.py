#!/usr/bin/env python3
"""Consolidate *externally reviewed* micro-expert traces into a Qwen SFT dataset.

This tool DOES NOT run experts, verify real execution, audit redaction, or train.
A separate operator-reviewed SHA256 allowlist is required so a model cannot
label its own generated trace as verified. Trust the origin of that allowlist.
"""
import argparse
import hashlib
import json
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent

def canonical(obj):
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)

def digest(obj):
    return hashlib.sha256(canonical(obj).encode("utf-8")).hexdigest()

def check_tool_trajectory(record, admitted_names):
    messages = record.get("messages")
    if not isinstance(messages, list) or len(messages) < 3:
        raise ValueError("Empty or malformed message history")
    if messages[0].get("role") != "system" or messages[1].get("role") != "user":
        raise ValueError("Missing system/user prefix")
    if messages[-1].get("role") != "assistant":
        raise ValueError("Incomplete trajectory")
    pending = {}
    tool_count = 0
    for message in messages[2:]:
        role = message.get("role")
        if role == "assistant" and message.get("tool_calls"):
            if pending:
                raise ValueError("New calls before previous tool results")
            for call in message["tool_calls"]:
                fn = call.get("function") or {}
                name = fn.get("name")
                if call.get("type") != "function" or name not in admitted_names:
                    raise ValueError("Unadmitted tool")
                if not isinstance(fn.get("arguments"), dict):
                    raise ValueError("Arguments must be native JSON objects")
                call_id = call.get("id")
                if not isinstance(call_id, str) or not call_id or call_id in pending:
                    raise ValueError("Invalid or duplicate call id")
                pending[call_id] = name
                tool_count += 1
        elif role == "tool":
            call_id = message.get("tool_call_id")
            if not call_id or pending.pop(call_id, None) != message.get("name"):
                raise ValueError("Mismatched tool result")
            if not isinstance(message.get("content"), str):
                raise ValueError("Tool content is not a string")
        elif role == "assistant":
            if pending:
                raise ValueError("Incomplete tool result turn")
        else:
            raise ValueError("Unexpected role " + str(role))
    if pending:
        raise ValueError("Tool result(s) missing")
    return tool_count

def consolidate(rows, reviewed_hashes, expert_ids):
    result = {"train": [], "eval": []}
    families = {}
    ids = set()
    for record in rows:
        trace_hash = digest(record)
        if trace_hash not in reviewed_hashes:
            raise ValueError("Trace not operator-allowlisted: " + trace_hash[:12])
        rid = record.get("trace_id")
        if not isinstance(rid, str) or not rid or rid in ids:
            raise ValueError("Missing/duplicate trace ID")
        ids.add(rid)
        if record.get("runtime_verified") is not True or record.get("task_passed") is not True:
            raise ValueError("Runtime-verified successful trajectory required")
        if record.get("redaction_reviewed") is not True:
            raise ValueError("Redaction review required")
        annotations = record.get("micro_annotations")
        if not isinstance(annotations, list) or not annotations:
            raise ValueError("No micro-expert annotations")
        if any(x.get("expert") not in expert_ids or not isinstance(x.get("decision"), dict)
               for x in annotations if isinstance(x, dict)):
            raise ValueError("Unknown/invalid micro-expert annotation")
        if not all(isinstance(x, dict) for x in annotations):
            raise ValueError("Malformed micro-expert annotations")
        split, family = record.get("split"), record.get("family")
        if split not in result or not isinstance(family, str) or not family:
            raise ValueError("Missing split/family")
        if family in families and families[family] != split:
            raise ValueError("Family leakage between train and evaluation")
        families[family] = split
        tools = record.get("tools")
        if not isinstance(tools, list):
            raise ValueError("Missing live tool definitions")
        names = [t["function"]["name"] for t in tools if isinstance(t, dict)
                 and t.get("type") == "function" and isinstance(t.get("function"), dict)
                 and "name" in t["function"]]
        if len(names) != len(tools) or len(names) != len(set(names)):
            raise ValueError("Invalid or duplicated tool schemas")
        count = check_tool_trajectory(record, set(names))
        result[split].append({
            "messages": record["messages"], "tools": tools,
            "metadata": {
                "family": family, "trace_hash": trace_hash, "trace_id": rid,
                "source": "externally_operator_reviewed_micro_expert_run",
                "tool_calls": count,
            }
        })
    if not result["train"] or not result["eval"]:
        raise ValueError("Need independently reviewed examples in both splits")
    return result

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trace-file", required=True,
                    help="Trusted runtime-exported and already redacted JSONL")
    ap.add_argument("--reviewed-sha256-file", required=True,
                    help="Operator-reviewed SHA256 digests of exact JSONL records")
    ap.add_argument("--output-dir", default=str(ROOT / "student_verified_data"))
    args = ap.parse_args()
    manifest = json.loads((ROOT / "micro_experts.json").read_text())
    expert_ids = {x["id"] for x in manifest["experts"]}
    rows = [json.loads(line) for line in Path(args.trace_file).read_text().splitlines()
            if line.strip()]
    reviewed = set(Path(args.reviewed_sha256_file).read_text().split())
    result = consolidate(rows, reviewed, expert_ids)
    output = Path(args.output_dir)
    output.mkdir(parents=True, exist_ok=True)
    for split, examples in result.items():
        (output / (split + ".jsonl")).write_text(
            "".join(json.dumps(x, sort_keys=True, ensure_ascii=False) + "\n" for x in examples)
        )
    (output / "manifest.json").write_text(json.dumps({
        "student": manifest["target_student_checkpoint"],
        "counts": {split: len(examples) for split, examples in result.items()},
        "provenance": "operator-reviewed SHA allowlist; runtime/source attestation is external",
        "warning": "This script validates structure, NOT actual tool execution or redaction",
    }, indent=2) + "\n")
    print("Student examples prepared. No model training or real verification performed.")

if __name__ == "__main__":
    main()
