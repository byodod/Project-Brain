from __future__ import annotations

import json
import pathlib
import tarfile
import tempfile
import unittest
import zipfile

import sys


SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))

import assemble_npm_package as npm_package  # noqa: E402


class AssembleNpmPackageTests(unittest.TestCase):
    def test_assembles_all_targets_and_rewrites_only_the_staged_manifest(self) -> None:
        repo = SCRIPTS.parent
        version = npm_package.workspace_version(repo / "Cargo.toml")
        with tempfile.TemporaryDirectory(prefix="project-brain-npm-test-") as temporary:
            root = pathlib.Path(temporary)
            dist = root / "dist"
            output = root / "package"
            dist.mkdir()
            expected = {}
            for target, _, _, extension, executable in npm_package.TARGETS:
                data = f"fixture:{target}".encode()
                expected[target] = data
                archive = dist / f"project-brain-{version}-{target}.{extension}"
                member = f"project-brain-{version}-{target}/{executable}"
                if extension == "zip":
                    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
                        bundle.writestr(member, data)
                else:
                    payload = root / executable
                    payload.write_bytes(data)
                    with tarfile.open(archive, "w:gz") as bundle:
                        bundle.add(payload, arcname=member)

            npm_package.assemble(repo, dist, output, version)

            package = json.loads((output / "package.json").read_text(encoding="utf-8"))
            self.assertEqual(package["name"], npm_package.PACKAGE_NAME)
            self.assertEqual(package["version"], version)
            self.assertEqual(package["license"], "MIT OR Apache-2.0")
            self.assertEqual(package["author"], "byodod and Project Brain contributors")
            self.assertNotIn("private", package)
            manifest = json.loads((output / "vendor" / "manifest.json").read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["binaries"]), 4)
            for entry in manifest["binaries"]:
                self.assertEqual((output / entry["file"]).read_bytes(), expected[entry["target"]])
            self.assertFalse((output / "test").exists())
            for license_name in ("LICENSE", "LICENSE-MIT", "LICENSE-APACHE"):
                self.assertEqual((output / license_name).read_bytes(), (repo / license_name).read_bytes())

    def test_rejects_version_mismatch_and_existing_output(self) -> None:
        repo = SCRIPTS.parent
        with tempfile.TemporaryDirectory(prefix="project-brain-npm-test-") as temporary:
            root = pathlib.Path(temporary)
            with self.assertRaisesRegex(ValueError, "不一致"):
                npm_package.assemble(repo, root, root / "output", "99.0.0")
            output = root / "exists"
            output.mkdir()
            version = npm_package.workspace_version(repo / "Cargo.toml")
            with self.assertRaisesRegex(ValueError, "已存在"):
                npm_package.assemble(repo, root, output, version)


if __name__ == "__main__":
    unittest.main()
