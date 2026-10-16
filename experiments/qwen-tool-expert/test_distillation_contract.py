#!/usr/bin/env python3
"""Unit tests for the structural student-distillation gate (no real tool execution)."""
import copy
import json
import unittest
from pathlib import Path

from distill_expert_runs import consolidate, digest

ROOT = Path(__file__).resolve().parent

def trace(index, split):
    catalog = json.loads((ROOT / "tool_catalog.seed.json").read_text())
    tool = next(x["interface"] for x in catalog["tools"] if x["name"] == "fs.read")
    return {
        "trace_id": "synthetic-contract-" + str(index),
        "family": "family-" + str(index),
        "split": split,
        "runtime_verified": True,   # Synthetic fixture; NOT actual execution.
        "task_passed": True,         # Synthetic fixture; NOT actual evaluation.
        "redaction_reviewed": True,  # Synthetic fixture; NOT actual review.
        "micro_annotations": [{"expert": "intent", "decision": {"action": "tool"}}],
        "tools": [tool],
        "messages": [
            {"role": "system", "content": "Use valid tools."},
            {"role": "user", "content": "Read file."},
            {"role": "assistant", "tool_calls": [{
                "id": "call_1", "type": "function",
                "function": {"name": "fs.read", "arguments": {"path": "README.md"}},
            }]},
            {"role": "tool", "name": "fs.read", "tool_call_id": "call_1",
             "content": '{"text":"# synthetic example"}'},
            {"role": "assistant", "content": "Read the example."},
        ],
    }

class DistillationContractTests(unittest.TestCase):
    def setUp(self):
        self.train = trace(1, "train")
        self.eval = trace(2, "eval")
        self.experts = {"intent"}

    def test_accepts_structurally_valid_operator_allowlisted_fixture(self):
        result = consolidate([self.train, self.eval],
                             {digest(self.train), digest(self.eval)}, self.experts)
        self.assertEqual(len(result["train"]), 1)
        self.assertEqual(len(result["eval"]), 1)
        self.assertEqual(result["train"][0]["metadata"]["tool_calls"], 1)

    def test_rejects_modified_trace_without_new_review(self):
        allowlist = {digest(self.train), digest(self.eval)}
        changed = copy.deepcopy(self.train)
        changed["messages"][-1]["content"] = "Modified after review."
        with self.assertRaises(ValueError):
            consolidate([changed, self.eval], allowlist, self.experts)

    def test_rejects_missing_runtime_verification(self):
        changed = copy.deepcopy(self.train)
        changed["runtime_verified"] = False
        with self.assertRaises(ValueError):
            consolidate([changed, self.eval],
                        {digest(changed), digest(self.eval)}, self.experts)

    def test_rejects_tool_result_with_wrong_call_id(self):
        changed = copy.deepcopy(self.train)
        changed["messages"][3]["tool_call_id"] = "wrong"
        with self.assertRaises(ValueError):
            consolidate([changed, self.eval],
                        {digest(changed), digest(self.eval)}, self.experts)

    def test_rejects_family_leakage(self):
        changed = copy.deepcopy(self.eval)
        changed["family"] = self.train["family"]
        with self.assertRaises(ValueError):
            consolidate([self.train, changed],
                        {digest(self.train), digest(changed)}, self.experts)

    def test_rejects_model_without_micro_annotations(self):
        changed = copy.deepcopy(self.train)
        changed["micro_annotations"] = []
        with self.assertRaises(ValueError):
            consolidate([changed, self.eval],
                        {digest(changed), digest(self.eval)}, self.experts)

if __name__ == "__main__":
    unittest.main()
