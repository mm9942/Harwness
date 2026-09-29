import hashlib
import hmac
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "harw-github-webhook.py"
SPEC = importlib.util.spec_from_file_location("harw_github_webhook", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class TriggerTests(unittest.TestCase):
    def settings(self):
        root = Path(tempfile.mkdtemp())
        return MODULE.Settings(
            bind="127.0.0.1",
            port=8788,
            path="/github",
            repo="mm9942/Harwness",
            trigger="@harw paper",
            packet="paper/HARW_WORK_PACKET.md",
            repo_root=root,
            state_root=root / "state",
            allowed_actors=frozenset({"mm9942"}),
            branch_prefixes=("paper/",),
            secret=b"secret",
        )

    def payload(self, body="@harw paper", actor="mm9942", association="OWNER"):
        return {
            "action": "created",
            "repository": {"full_name": "mm9942/Harwness"},
            "issue": {"number": 75, "pull_request": {"url": "x"}},
            "comment": {
                "body": body,
                "author_association": association,
                "user": {"login": actor},
            },
        }

    def test_signature_accepts_valid_hmac(self):
        body = b'{"ok":true}'
        digest = hmac.new(b"secret", body, hashlib.sha256).hexdigest()
        self.assertTrue(
            MODULE.verify_signature(b"secret", body, f"sha256={digest}")
        )

    def test_signature_rejects_missing_or_wrong(self):
        body = b"{}"
        self.assertFalse(MODULE.verify_signature(b"secret", body, None))
        self.assertFalse(MODULE.verify_signature(b"secret", body, "sha256=" + "0" * 64))

    def test_exact_trigger_is_admitted(self):
        trigger = MODULE.parse_trigger(
            payload=self.payload(),
            delivery_id="abc-123",
            event="issue_comment",
            settings=self.settings(),
        )
        self.assertEqual(trigger.pr, 75)
        self.assertEqual(trigger.actor, "mm9942")

    def test_non_trigger_comment_is_ignored(self):
        trigger = MODULE.parse_trigger(
            payload=self.payload("please write the paper"),
            delivery_id="abc-123",
            event="issue_comment",
            settings=self.settings(),
        )
        self.assertIsNone(trigger)

    def test_non_pr_comment_is_ignored(self):
        payload = self.payload()
        payload["issue"].pop("pull_request")
        trigger = MODULE.parse_trigger(
            payload=payload,
            delivery_id="abc-123",
            event="issue_comment",
            settings=self.settings(),
        )
        self.assertIsNone(trigger)

    def test_untrusted_actor_is_rejected(self):
        with self.assertRaises(MODULE.TriggerError):
            MODULE.parse_trigger(
                payload=self.payload(actor="mallory"),
                delivery_id="abc-123",
                event="issue_comment",
                settings=self.settings(),
            )

    def test_untrusted_association_is_rejected(self):
        with self.assertRaises(MODULE.TriggerError):
            MODULE.parse_trigger(
                payload=self.payload(association="NONE"),
                delivery_id="abc-123",
                event="issue_comment",
                settings=self.settings(),
            )

    def test_other_repository_is_rejected(self):
        payload = self.payload()
        payload["repository"]["full_name"] = "other/repo"
        with self.assertRaises(MODULE.TriggerError):
            MODULE.parse_trigger(
                payload=payload,
                delivery_id="abc-123",
                event="issue_comment",
                settings=self.settings(),
            )

    def test_slash_alias_is_exact(self):
        trigger = MODULE.parse_trigger(
            payload=self.payload("/harw paper"),
            delivery_id="abc-123",
            event="issue_comment",
            settings=self.settings(),
        )
        self.assertIsNotNone(trigger)

    def test_review_approved_accepts_standard_reviewer_verdict(self):
        self.assertTrue(MODULE.review_approved("**Urteil:** freigegeben\n"))
        self.assertFalse(MODULE.review_approved("**Urteil:** überarbeiten\n"))

    def test_paper_only_guard_rejects_non_paper_paths(self):
        MODULE.ensure_paper_only(["paper/drafts/a.md", "paper/src/main.tex"])
        with self.assertRaises(MODULE.TriggerError):
            MODULE.ensure_paper_only(["paper/drafts/a.md", "src/lib.rs"])


if __name__ == "__main__":
    unittest.main()
