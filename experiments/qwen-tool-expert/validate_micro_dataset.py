#!/usr/bin/env python3
"""Validate phase-supervision labels, train/eval isolation and tool contracts."""
import argparse
import hashlib
import json
from pathlib import Path

from build_dataset import validate_call

ROOT = Path(__file__).resolve().parent

def validate(folder):
    root = Path(folder)
    micro = json.loads((ROOT / "micro_experts.json").read_text())
    manifest = json.loads((root / "manifest.json").read_text())
    catalog_bytes = (ROOT / "tool_catalog.seed.json").read_bytes()
    catalog = {x["name"]: x for x in json.loads(catalog_bytes)["tools"]}
    assert manifest["tool_catalog_sha256"] == hashlib.sha256(catalog_bytes).hexdigest()
    assert manifest["checkpoint"] == micro["base_checkpoint"]
    pilot = {x["id"] for x in micro["experts"] if x["pilot"]}
    assert set(manifest["counts"]) == pilot
    family_splits = {}
    counts = {}
    for expert in sorted(pilot):
        counts[expert] = {}
        for split in ("train", "eval"):
            path = root / expert / (split + ".jsonl")
            rows = [json.loads(line) for line in path.read_text().splitlines() if line]
            counts[expert][split] = len(rows)
            assert rows, (expert, split, "empty dataset")
            for row in rows:
                meta = row["metadata"]
                assert meta["expert"] == expert
                assert meta["split"] == split
                assert meta["synthetic"] is True and meta["execution_verified"] is False
                family = meta["family"]
                assert family_splits.setdefault(family, split) == split, "family leakage"
                messages = row["messages"]
                assert [x["role"] for x in messages] == ["system", "user", "assistant"]
                features = json.loads(messages[1]["content"])
                target = json.loads(messages[2]["content"])
                assert isinstance(features, dict) and isinstance(target, dict)
                if expert == "intent":
                    assert set(target) == {"action"} and target["action"] in {"tool", "no_tool"}
                elif expert == "tool_selection":
                    assert set(target) == {"tool_name"}
                    assert target["tool_name"] is None or target["tool_name"] in {
                        t["name"] for t in features["available_tools"]
                    }
                elif expert == "argument_builder":
                    assert set(target) == {"tool_name", "arguments"}
                    name = target["tool_name"]
                    assert name in catalog and name == features["tool"]["name"]
                    assert features["tool"]["parameters"] == catalog[name]["interface"]["function"]["parameters"]
                    validate_call(catalog[name], target["arguments"])
                elif expert == "observation":
                    assert set(target) == {"status", "trust"}
                    assert target["status"] in {"usable", "degraded", "partial", "error"}
                    assert target["trust"] == "untrusted"
                    assert features["tool_name"] in catalog
                elif expert == "continuation":
                    assert set(target) == {"action", "next_tool"}
                    assert target["action"] in {"call_tool", "finish"}
                    assert (target["action"] == "finish") == (target["next_tool"] is None)
                    if target["next_tool"] is not None:
                        assert target["next_tool"] in {t["name"] for t in features["available_tools"]}
                elif expert == "verification":
                    assert set(target) == {"next_verification", "evidence_status"}
                    assert target["next_verification"] in {"none", "read_back", "investigate"}
                    assert target["evidence_status"] == "synthetic_not_executed"
                    assert features["tool_name"] in catalog
                else:
                    raise AssertionError("Unknown expert " + expert)
    assert counts == manifest["counts"], "Manifest count mismatch"
    assert set(manifest["eval_families"]).isdisjoint({
        family for family, split in family_splits.items() if split == "train"
    })
    return {"result": "PASS", "counts": counts, "families": len(family_splits),
            "note": "Tests labels and contracts only, not pretrained model accuracy."}

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-dir", default=str(ROOT / "micro_data"))
    args = parser.parse_args()
    print(json.dumps(validate(args.input_dir), indent=2))

if __name__ == "__main__":
    main()
