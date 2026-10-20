from __future__ import annotations

import hashlib
import os
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path
from unittest.mock import patch

import repair_dmg
import dmg_signing
from ds_store_layout import assert_layout


class RepairDmgTests(unittest.TestCase):
    def test_identity_matching_accepts_only_exact_name_or_fingerprint(self) -> None:
        output = '  1) ABCDEF0123456789ABCDEF0123456789ABCDEF01 "Developer ID Application: Peppy"\n'
        with patch("dmg_signing.security_run", return_value=output):
            self.assertTrue(dmg_signing.find_identity("Developer ID Application: Peppy"))
            self.assertTrue(dmg_signing.find_identity("ABCDEF0123456789ABCDEF0123456789ABCDEF01"))
            self.assertFalse(dmg_signing.find_identity("Developer ID Application"))

    def test_developer_id_uses_explicit_existing_keychain_identity(self) -> None:
        calls: list[tuple[str, ...]] = []

        def security(*command: str, capture: bool = False) -> str:
            calls.append(command)
            return '  1) ABCDEF0123456789ABCDEF0123456789ABCDEF01 "Developer ID Application: Peppy"\n'

        with patch("dmg_signing.security_run", side_effect=security), patch.dict(os.environ, {}, clear=True):
            with dmg_signing.developer_id_signing("Developer ID Application: Peppy") as signing:
                self.assertIsNone(signing.keychain)
                self.assertEqual(signing.identity, "Developer ID Application: Peppy")

        self.assertEqual(calls, [("find-identity", "-v", "-p", "codesigning")])

    def test_developer_id_imports_certificate_into_owned_keychain_and_removes_it(self) -> None:
        calls: list[tuple[str, ...]] = []

        def security(*command: str, capture: bool = False) -> str:
            calls.append(command)
            if command[:3] == ("find-identity", "-v", "-p"):
                if len(command) == 4:
                    return "0 valid identities found\n"
                return '  1) ABCDEF0123456789ABCDEF0123456789ABCDEF01 "Developer ID Application: Peppy"\n'
            if command[:2] == ("list-keychains", "-d"):
                return '    "login.keychain-db"\n'
            return ""

        environment = {
            "APPLE_CERTIFICATE": "cDEy",
            "APPLE_CERTIFICATE_PASSWORD": "certificate-password",
        }
        with patch("dmg_signing.security_run", side_effect=security), patch.dict(os.environ, environment, clear=True):
            with dmg_signing.developer_id_signing("ABCDEF0123456789ABCDEF0123456789ABCDEF01") as signing:
                self.assertIsNotNone(signing.keychain)
                self.assertTrue(signing.keychain.name.endswith(".keychain-db"))
                self.assertEqual(oct(signing.certificate.stat().st_mode & 0o777), "0o600")
                keychain_directory = signing.keychain.parent

        self.assertTrue(any(call[:1] == ("create-keychain",) for call in calls))
        self.assertTrue(any(call[:1] == ("set-key-partition-list",) for call in calls))
        self.assertTrue(any(call[:1] == ("delete-keychain",) for call in calls))
        self.assertFalse(any(call[:2] == ("list-keychains", "-s") for call in calls))
        self.assertFalse(keychain_directory.exists())

    def test_developer_id_import_failure_cleans_owned_keychain_without_secret_output(self) -> None:
        calls: list[tuple[str, ...]] = []

        def security(*command: str, capture: bool = False) -> str:
            calls.append(command)
            if command[:3] == ("find-identity", "-v", "-p"):
                return "0 valid identities found\n"
            if command[:1] == ("import",):
                raise dmg_signing.SigningError("certificate import failed")
            if command[:2] == ("list-keychains", "-d"):
                return '    "login.keychain-db"\n'
            return ""

        environment = {
            "APPLE_CERTIFICATE": "cDEy",
            "APPLE_CERTIFICATE_PASSWORD": "certificate-password",
        }
        with patch("dmg_signing.security_run", side_effect=security), patch.dict(os.environ, environment, clear=True):
            with self.assertRaisesRegex(dmg_signing.SigningError, "certificate import failed") as error:
                with dmg_signing.developer_id_signing("Developer ID Application: Peppy"):
                    pass

        self.assertNotIn("certificate-password", str(error.exception))
        self.assertTrue(any(call[:1] == ("delete-keychain",) for call in calls))

    def test_parseable_multiple_mounts_detaches_only_returned_roots(self) -> None:
        response = plistlib.dumps({"system-entities": [
            {"dev-entry": "/dev/disk71s1", "mount-point": "/owned/one"},
            {"dev-entry": "/dev/disk72s1", "mount-point": "/owned/two"},
        ]}).decode()
        with patch("repair_dmg.devices_for_image", return_value=set()), patch("repair_dmg.run", return_value=response), patch("repair_dmg.detach") as detach:
            with self.assertRaisesRegex(RuntimeError, "exactly one"):
                repair_dmg.attached_devices(Path("/owned/image.dmg"), Path("/owned/mount"), True)
        detach.assert_has_calls([unittest.mock.call("/dev/disk71"), unittest.mock.call("/dev/disk72")], any_order=True)

    def test_malformed_attach_metadata_detaches_only_newly_owned_devices(self) -> None:
        with patch("repair_dmg.devices_for_image", side_effect=[set(), {"/dev/disk81"}]), patch("repair_dmg.run", return_value="not a plist"), patch("repair_dmg.detach") as detach:
            with self.assertRaisesRegex(RuntimeError, "malformed"):
                repair_dmg.attached_devices(Path("/owned/image.dmg"), Path("/owned/mount"), True)
        detach.assert_called_once_with("/dev/disk81")

    def test_convert_failure_leaves_original_and_no_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "image.dmg"
            image.write_bytes(b"original")

            @contextmanager
            def mounted(_image: Path, _root: Path, writable: bool):
                yield "/dev/disk91s1", Path(temporary)

            with patch("repair_dmg.mounted_volume", mounted), patch("repair_dmg.app_manifest", return_value={}), patch("repair_dmg.write_layout"), patch("repair_dmg.assert_layout"), patch("repair_dmg.run", side_effect=subprocess.CalledProcessError(1, "hdiutil")), patch("dmg_signing.security_run") as security:
                with self.assertRaises(subprocess.CalledProcessError):
                    repair_dmg.repair(image, "adhoc", None)
            security.assert_not_called()
            self.assertEqual(image.read_bytes(), b"original")
            self.assertFalse(list(Path(temporary).glob(".peppy-dmg-*")))

    def test_developer_id_without_certificate_fails_before_hdiutil(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "image.dmg"
            image.write_bytes(b"original")
            with patch("dmg_signing.security_run", return_value="0 valid identities found") as security, patch.dict(os.environ, {}, clear=True):
                with self.assertRaisesRegex(ValueError, "certificate inputs"):
                    repair_dmg.repair(image, "developer-id", "Developer ID Application: Missing")
            security.assert_called_once_with("find-identity", "-v", "-p", "codesigning", capture=True)
            self.assertEqual(image.read_bytes(), b"original")

    def test_developer_id_sign_failure_keeps_original_and_uses_private_keychain(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "image.dmg"
            image.write_bytes(b"original")
            keychain = Path(temporary) / "repair.keychain-db"

            @contextmanager
            def mounted(_image: Path, _root: Path, writable: bool):
                yield "/dev/disk91s1", Path(temporary)

            @contextmanager
            def signing(_identity: str | None):
                yield dmg_signing.SigningIdentity("Developer ID Application: Peppy", keychain)

            def command(*arguments: str, capture: bool = False) -> str:
                if arguments[:2] == ("hdiutil", "convert"):
                    Path(arguments[arguments.index("-o") + 1]).write_bytes(b"candidate")
                if arguments[:2] == ("hdiutil", "imageinfo"):
                    return plistlib.dumps({"Format": "UDZO"}).decode()
                if arguments[0] == "codesign":
                    raise subprocess.CalledProcessError(1, "codesign")
                return ""

            with patch("repair_dmg.developer_id_signing", signing), patch("repair_dmg.mounted_volume", mounted), patch("repair_dmg.app_manifest", return_value={}), patch("repair_dmg.write_layout"), patch("repair_dmg.assert_layout"), patch("repair_dmg.run", side_effect=command) as run:
                with self.assertRaises(subprocess.CalledProcessError):
                    repair_dmg.repair(image, "developer-id", "Developer ID Application: Peppy")
            sign_call = next(call.args for call in run.call_args_list if call.args[0] == "codesign")
            self.assertIn("--keychain", sign_call)
            self.assertIn(str(keychain), sign_call)
            self.assertEqual(image.read_bytes(), b"original")
            self.assertFalse(list(Path(temporary).glob(".peppy-dmg-*")))

    @unittest.skipUnless(os.uname().sysname == "Darwin", "requires macOS hdiutil")
    def test_hfs_round_trip_keeps_payload_and_persists_real_layout(self) -> None:
        with tempfile.TemporaryDirectory(prefix="peppy-dmg-test-") as temporary:
            root = Path(temporary)
            image = root / "fixture.dmg"
            first_mount = root / "first-mount"
            verify_mount = root / "verify-mount"
            volume_name = "PeppyRepairFixture"
            subprocess.run(("hdiutil", "create", "-quiet", "-size", "20m", "-fs", "HFS+", "-volname", volume_name, "-ov", str(image)), check=True)
            first_mount.mkdir()
            subprocess.run(("hdiutil", "attach", "-quiet", "-nobrowse", "-mountpoint", str(first_mount), str(image)), check=True)
            try:
                app = first_mount / "Fixture.app" / "Contents"
                app.mkdir(parents=True)
                payload = app / "payload"
                payload.write_bytes(b"regular payload")
                payload.chmod(0o755)
                (app / "payload-link").symlink_to("payload")
                (first_mount / "Applications").symlink_to("/Applications")
                background = Path(__file__).parents[1] / "background.tiff"
                (first_mount / ".background").mkdir()
                shutil.copy2(background, first_mount / ".background" / "background.tiff")
            finally:
                subprocess.run(("hdiutil", "detach", "-quiet", str(first_mount)), check=True)
            before_hash = hashlib.sha256(image.read_bytes()).hexdigest()
            repair_dmg.repair(image, "adhoc", None)
            self.assertNotEqual(hashlib.sha256(image.read_bytes()).hexdigest(), before_hash)
            self.assertFalse(list(root.glob(".peppy-dmg-*")))
            subprocess.run(("hdiutil", "verify", str(image)), check=True, stdout=subprocess.DEVNULL)
            verify_mount.mkdir()
            attached = subprocess.run(("hdiutil", "attach", "-plist", "-readonly", "-nobrowse", "-mountpoint", str(verify_mount), str(image)), check=True, capture_output=True)
            try:
                entities = plistlib.loads(attached.stdout)["system-entities"]
                device = next(entity["dev-entry"] for entity in entities if entity.get("mount-point"))
                assert_layout(verify_mount, device)
                manifest = repair_dmg.app_manifest(verify_mount)
                self.assertIn("Fixture.app/Contents/payload-link", manifest)
                self.assertIn("link:", manifest["Fixture.app/Contents/payload-link"])
            finally:
                subprocess.run(("hdiutil", "detach", "-quiet", str(verify_mount)), check=True)


if __name__ == "__main__":
    unittest.main()
