#!/usr/bin/env python3
"""Validate generated tool-use SFT trajectories before any costly training."""
import argparse
import json
import pathlib

from build_dataset import validate_call

ROOT = pathlib.Path(__file__).resolve().parent

def evaluate(folder):
    folder = pathlib.Path(folder)
    catalog = json.loads((ROOT / "tool_catalog.seed.json").read_text())
    tools_by_name = {x["name"]: x for x in catalog["tools"]}
    families = {}
    counts = {}
    for split in ("train", "eval"):
        rows = [json.loads(line) for line in (folder / (split + ".jsonl")).read_text().splitlines() if line.strip()]
        counts[split] = len(rows)
        for row in rows:
            fam = row["metadata"]["family"]
            if fam in families and families[fam] != split: raise AssertionError("Family leakage: " + fam)
            families[fam] = split
            if row["metadata"]["synthetic"] is not True: raise AssertionError("Not synthetic")
            available = {tool["function"]["name"] for tool in row["tools"]}
            if len(available) != len(row["tools"]): raise AssertionError("Duplicate tools")
            assert available <= set(tools_by_name)
            for tool in row["tools"]:
                name = tool["function"]["name"]
                if tool != tools_by_name[name]["interface"]: raise AssertionError("Tool spec drift: "+name)
            messages = row["messages"]
            if len(messages) < 3 or messages[0]["role"] != "system" or messages[1]["role"] != "user": raise AssertionError("Missing prompt")
            assert messages[-1]["role"] == "assistant" and messages[-1].get("content")
            pending = None
            for message in messages[2:]:
                role = message["role"]
                if role == "assistant" and "tool_calls" in message:
                    if pending is not None: raise AssertionError("Unpaired call")
                    calls = message["tool_calls"]
                    if len(calls) != 1: raise AssertionError("Expected one tool per step")
                    call = calls[0]
                    name = call["function"]["name"]
                    if name not in available: raise AssertionError("Unadmitted tool "+name)
                    validate_call(tools_by_name[name], json.loads(call["function"]["arguments"]))
                    pending = (call["id"], name)
                elif role == "tool":
                    if pending != (message.get("tool_call_id"),message.get("name")): raise AssertionError("Wrong tool reply")
                    json.loads(message["content"])
                    pending = None
                elif role == "assistant":
                    if pending is not None: raise AssertionError("Missing tool reply")
                else: raise AssertionError("Unexpected role "+role)
            if pending: raise AssertionError("Missing last reply")
    if not counts["train"] or not counts["eval"]: raise AssertionError("Empty splits")
    return {"result":"PASS", "counts":counts,"families":len(families),"catalog_tools":len(tools_by_name)}

if __name__=="__main__":
    parser=argparse.ArgumentParser()
    parser.add_argument("--input-dir",default=str(ROOT/"data"))
    args=parser.parse_args()
    print(json.dumps(evaluate(args.input_dir),indent=2))
