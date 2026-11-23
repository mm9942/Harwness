"""No signing, credentials, or network is used to test the FCM publish hook."""

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import fcm_send


class FcmPublishTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.key_file = Path(directory.name) / "service-account.json"
        self.start_patch(patch.object(fcm_send, "KEY", str(self.key_file)))
        self.start_patch(patch.object(fcm_send.sys, "argv", ["fcm_send.py", "315", "0.3.15"]))
        self.start_patch(patch("builtins.print"))
        self.access_token = self.start_patch(
            patch.object(fcm_send, "access_token", return_value="unit-test-token")
        )
        self.urlopen = self.start_patch(patch.object(fcm_send.urllib.request, "urlopen"))
        self.urlopen.return_value.__enter__.return_value.read.return_value = b"{}"

    def start_patch(self, patcher):
        self.addCleanup(patcher.stop)
        return patcher.start()

    def set_account(self, document):
        self.key_file.write_text(json.dumps(document), encoding="utf-8")

    def test_invalid_project_is_rejected_before_signing_or_network(self):
        invalid_accounts = [
            [],
            {},
            {"project_id": None},
            {"project_id": 123456},
            {"project_id": "short"},
            {"project_id": "Avalid"},
            {"project_id": "testapp-"},
            {"project_id": "test_app"},
            {"project_id": "testapp/qa"},
            {"project_id": "testapp?qa"},
            {"project_id": "testapp#qa"},
            {"project_id": "testapp%2Fqa"},
            {"project_id": "test app"},
            {"project_id": "testapp\n"},
            {"project_id": "testappé"},
            {"project_id": "a" * 31},
        ]
        for account in invalid_accounts:
            with self.subTest(account=account):
                self.set_account(account)
                with self.assertRaises(ValueError):
                    fcm_send.main()

        self.access_token.assert_not_called()
        self.urlopen.assert_not_called()

    def test_valid_project_uses_fixed_fcm_endpoint_and_update_topic(self):
        for project in ("a-2345", "harw-app-123", "a" + "1" * 29):
            with self.subTest(project=project):
                account = {"project_id": project}
                self.set_account(account)
                fcm_send.main()
                self.access_token.assert_called_with(account)
                request = self.urlopen.call_args.args[0]
                self.assertEqual(
                    request.full_url, f"https://fcm.googleapis.com/v1/projects/{project}/messages:send"
                )
                self.assertEqual(request.get_method(), "POST")
                self.assertEqual(request.get_header("Authorization"), "Bearer unit-test-token")
                self.assertEqual(json.loads(request.data)["message"]["topic"], "updates")

    def test_no_key_skips_push(self):
        fcm_send.main()
        self.access_token.assert_not_called()
        self.urlopen.assert_not_called()
