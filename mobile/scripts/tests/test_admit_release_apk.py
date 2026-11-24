import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve()
loader = importlib.machinery.SourceFileLoader("admission", str(HERE.parents[1] / "admit-release-apk"))
spec = importlib.util.spec_from_loader(loader.name, loader)
admission = importlib.util.module_from_spec(spec)
loader.exec_module(admission)

SHA = "a" * 40
SIGNER = "b" * 64
PREV = "c" * 64
VERSION = ("0.3.18", "21")


class AdmissionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.apk = self.base / "Harw-0.3.18-build21-ARM64.apk"
        self.apk.write_bytes(b"synthetic test APK")
        self.receipt = self.base / "receipt.json"
        self.digest = hashlib.sha256(self.apk.read_bytes()).hexdigest()
        self.receipt_body = {
            "schema": 1, "apk_sha256": self.digest, "source_commit": SHA,
            "smoke": {
                "clean_install_and_launch": "passed",
                "in_place_update_and_launch": "passed",
                "update_data_retention": "passed",
                "local_assistant_reply": "passed",
                "local_library_persistence": "passed",
                "update_from_apk_sha256": PREV,
            },
        }

    def write_receipt(self):
        blob = json.dumps(self.receipt_body).encode()
        self.receipt.write_bytes(blob)
        return hashlib.sha256(blob).hexdigest()

    def admit(self, pin=None, actual_signer=SIGNER, product=VERSION):
        actual_pin = self.write_receipt()
        pin = actual_pin if pin is None else pin
        with patch.object(admission, "git_version", return_value=VERSION), patch.object(
            admission, "inspect_apk", return_value=("dev.harw.mobile", *product, actual_signer)
        ):
            return admission.admit(self.apk, self.receipt, pin, SHA, SIGNER, PREV)

    def test_verified_pair_emits_one_standard_sha256_sidecar_line(self):
        self.assertEqual(self.admit(), f"{self.digest}  {self.apk.name}\n")

    def test_wrong_source_and_wrong_apk_are_rejected_even_with_matching_receipt_hash(self):
        self.receipt_body["source_commit"] = "d" * 40
        with self.assertRaisesRegex(ValueError, "source commit"):
            self.admit()
        self.receipt_body["source_commit"] = SHA
        self.apk.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "APK checksum"):
            self.admit()

    def test_pin_wrong_signer_and_committed_version(self):
        with self.assertRaisesRegex(ValueError, "receipt checksum"):
            self.admit(pin="0" * 64)
        with self.assertRaisesRegex(ValueError, "signer"):
            self.admit(actual_signer="1" * 64)
        with self.assertRaisesRegex(ValueError, "version"):
            self.admit(product=("0.3.20", "30"))

    def test_admission_demands_real_device_evidence_for_pinned_predecessor(self):
        self.receipt_body["smoke"]["in_place_update_and_launch"] = "not run"
        with self.assertRaisesRegex(ValueError, "device smoke"):
            self.admit()
        self.receipt_body["smoke"]["in_place_update_and_launch"] = "passed"
        self.receipt_body["smoke"]["update_from_apk_sha256"] = "f" * 64
        with self.assertRaisesRegex(ValueError, "predecessor"):
            self.admit()

    def test_admission_rejects_missing_local_assistant_or_library_device_evidence(self):
        for field in ("local_assistant_reply", "local_library_persistence"):
            with self.subTest(field=field):
                del self.receipt_body["smoke"][field]
                with self.assertRaisesRegex(ValueError, "Local Assistant or Library"):
                    self.admit()
                self.receipt_body["smoke"][field] = "passed"

    def test_inspection_fails_closed_for_multisigner_or_other_arch(self):
        badging = "package: name='dev.harw.mobile' versionCode='21' versionName='0.3.18'\nnative-code: 'arm64-v8a'\n"
        cert = f"Number of signers: 1\nSigner #1 certificate SHA-256 digest: {SIGNER}"
        with patch.object(admission, "command", side_effect=[cert + "\nSigner #2 certificate SHA-256 digest: " + PREV, badging]):
            with self.assertRaisesRegex(ValueError, "exactly one signer"):
                admission.inspect_apk(self.apk)
        with patch.object(admission, "command", side_effect=[cert, badging.replace("arm64-v8a", "x86_64")]):
            with self.assertRaisesRegex(ValueError, "ABI"):
                admission.inspect_apk(self.apk)

    def test_modern_apksigner_repeats_one_certificate_for_multiple_schemes(self):
        badging = "package: name='dev.harw.mobile' versionCode='21' versionName='0.3.18'\nnative-code: 'arm64-v8a'\n"
        certs = f"Number of signers: 1\nV2 Signer: certificate SHA-256 digest: {SIGNER}\nV3 Signer: certificate SHA-256 digest: {SIGNER}"
        with patch.object(admission, "command", side_effect=[certs, badging]):
            self.assertEqual(admission.inspect_apk(self.apk), ("dev.harw.mobile", *VERSION, SIGNER))


if __name__ == "__main__":
    unittest.main()
