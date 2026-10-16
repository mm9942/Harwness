#!/usr/bin/env python3
"""Offline unit tests for cycle supervision, no model weights or network."""
import hashlib
import json
import tempfile
import unittest
from pathlib import Path

from build_dataset import validate_call
from build_micro_dataset import build, EVAL_FAMILIES
from validate_micro_dataset import validate

ROOT = Path(__file__).resolve().parent

class MicroExpertDataTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.catalog_bytes = (ROOT / "tool_catalog.seed.json").read_bytes()
        cls.catalog = {x["name"]: x for x in json.loads(cls.catalog_bytes)["tools"]}
        cls.spec = json.loads((ROOT / "micro_experts.json").read_text())
        cls.pilot = [x["id"] for x in cls.spec["experts"] if x["pilot"]]

    def test_pilot_covers_each_phase_and_holdout(self):
        data = build(self.catalog, self.pilot)
        self.assertEqual(len(data), 6)
        for role, splits in data.items():
            self.assertTrue(splits["train"], role)
            self.assertTrue(splits["eval"], role)
            train_families = {r["metadata"]["family"] for r in splits["train"]}
            eval_families = {r["metadata"]["family"] for r in splits["eval"]}
            self.assertTrue(train_families.isdisjoint(eval_families))
            self.assertTrue(eval_families <= EVAL_FAMILIES)

    def test_argument_expert_is_closed_schema(self):
        data = build(self.catalog, self.pilot)
        for split in ("train", "eval"):
            for row in data["argument_builder"][split]:
                features = json.loads(row["messages"][1]["content"])
                target = json.loads(row["messages"][2]["content"])
                self.assertEqual(features["tool"]["name"], target["tool_name"])
                self.assertTrue(validate_call(
                    self.catalog[target["tool_name"]], target["arguments"]
                ))
        with self.assertRaises(ValueError):
            validate_call(self.catalog["lens.ask"], {
                "question": "x", "index_name": "admin"
            })

    def test_validator_round_trip(self):
        data = build(self.catalog, self.pilot)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            counts = {}
            for expert, splits in data.items():
                (root / expert).mkdir()
                counts[expert] = {}
                for split, rows in splits.items():
                    counts[expert][split] = len(rows)
                    (root / expert / (split + ".jsonl")).write_text(
                        "".join(json.dumps(row, sort_keys=True) + "\n" for row in rows)
                    )
            (root / "manifest.json").write_text(json.dumps({
                "format_version": 1, "checkpoint": self.spec["base_checkpoint"],
                "student": self.spec["target_student_checkpoint"],
                "source_git_ref": self.spec["pinned_harwness_dev"],
                "synthetic": True, "execution_verified": False, "counts": counts,
                "eval_families": sorted(EVAL_FAMILIES),
                "tool_catalog_sha256": hashlib.sha256(
                    self.catalog_bytes).hexdigest(),
            }))
            report = validate(root)
            self.assertEqual(report["result"], "PASS")
            # A guessed tool argument must be detected before any inference.
            p = root / "argument_builder" / "train.jsonl"
            examples = [json.loads(x) for x in p.read_text().splitlines()]
            label = json.loads(examples[0]["messages"][2]["content"])
            label["arguments"]["invented"] = "bad"
            examples[0]["messages"][2]["content"] = json.dumps(label)
            p.write_text("\n".join(json.dumps(x) for x in examples) + "\n")
            with self.assertRaises((ValueError, AssertionError)):
                validate(root)

if __name__ == "__main__":
    unittest.main()
