"""Offline producer regression tests; only disposable synthetic files are used."""

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import zipfile

import prepare_repository as producer


class PublicationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="publication-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.source = self.base / "source"
        self.source.mkdir()
        self.put("Cargo.toml", b'[package]\nname="synthetic"\nversion="0.1.0"\n')
        self.put("src/lib.rs", b"pub fn value() -> u8 { 1 }\n")

    def put(self, relative, data):
        path = self.source / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def staged(self, name="payload", metadata=None):
        planned = producer.plan(self.source, metadata)
        dest = self.base / name
        result = producer.stage(self.source, dest, planned["etag"], metadata)
        return dest, result

    def test_source_publication_keeps_tests_ci_and_normalizes_before_hashing(self):
        original = b"\xef\xbb\xbf# Example\r\nline\r"
        self.put("README.md", original)
        for path, body in (("tests/check.rs", b"#[test] fn check() {}\n"),
                           ("examples/demo.rs", b"fn main() {}\n"),
                           ("fixtures/request.json", b"{}\n"),
                           (".github/workflows/check.yml", b"name: Check\n"),
                           ("integrations/tool.json", b"{}\n")):
            self.put(path, body)
        for path in ("out/run.json", "docs/evidence/run.json", "AGENTS.md",
                     ".env", ".doctensa-coordinator/receipt.json", "target/file.rs"):
            self.put(path, b"excluded bytes need not be text\xff")
        dest, result = self.staged()
        self.assertEqual((dest / "README.md").read_bytes(), b"# Example\nline\n")
        self.assertEqual((self.source / "README.md").read_bytes(), original)
        self.assertTrue((dest / "tests/check.rs").is_file())
        self.assertTrue((dest / ".github/workflows/check.yml").is_file())
        self.assertFalse((dest / "AGENTS.md").exists())
        self.assertFalse((dest / "out").exists())
        self.assertEqual(result, producer.verify(dest))
        manifest = json.loads((dest / "FILES.json").read_bytes())
        item = next(x for x in manifest["files"] if x["path"] == "README.md")
        self.assertEqual(item["sha256"], producer.digest(b"# Example\nline\n"))
        self.assertEqual(item["size"], 15)

    def test_stale_and_wrong_representation_tags_have_zero_effects(self):
        old = producer.plan(self.source)
        self.put("src/lib.rs", b"pub fn value() -> u8 { 2 }\n")
        dest = self.base / "must-not-exist"
        with self.assertRaisesRegex(producer.Refusal, "etag-mismatch"):
            producer.stage(self.source, dest, old["etag"])
        self.assertFalse(dest.exists())
        current = producer.plan(self.source)
        with self.assertRaisesRegex(producer.Refusal, "etag-mismatch"):
            producer.stage(self.source, dest, current["inputEtag"])
        self.assertFalse(dest.exists())
        with self.assertRaisesRegex(producer.Refusal, "strong-etag-required"):
            producer.stage(self.source, dest, "W/" + current["etag"])
        self.assertFalse(dest.exists())

    def test_original_line_ending_change_invalidates_input_but_preserves_ordinary_tag(self):
        self.put("README.md", b"one\r\ntwo\r\n")
        before = producer.plan(self.source)
        self.put("README.md", b"one\ntwo\n")
        after = producer.plan(self.source)
        self.assertNotEqual(before["inputEtag"], after["inputEtag"])
        self.assertNotEqual(before["etag"], after["etag"])
        self.assertEqual(before["ordinaryEtag"], after["ordinaryEtag"])

    def test_existing_destinations_and_inside_source_are_not_modified(self):
        dest = self.base / "existing"
        dest.mkdir()
        sentinel = dest / "keep"
        sentinel.write_bytes(b"unchanged")
        tag = producer.plan(self.source)["etag"]
        with self.assertRaises(producer.Refusal):
            producer.stage(self.source, dest, tag)
        self.assertEqual(sentinel.read_bytes(), b"unchanged")
        with self.assertRaises(producer.Refusal):
            producer.stage(self.source, self.source / "publication", tag)
        self.assertFalse((self.source / "publication").exists())

    def test_private_content_is_refused_without_echoing_values(self):
        samples = [
            ("machine", "C:" + "/Users/" + "Example/private"),
            ("token", "gh" + "p_" + "a" * 36),
            ("assignment", "api_" + "key=" + "b" * 24),
            ("key", "-----BEGIN " + "PRIVATE KEY-----"),
            ("uuid", "01234567" + "-89ab-cdef-0123-456789abcdef"),
        ]
        for label, sample in samples:
            with self.subTest(label=label):
                self.put("README.md", ("public line\n" + sample + "\n").encode())
                with self.assertRaises(producer.Refusal) as caught:
                    producer.plan(self.source)
                self.assertNotIn(sample, str(caught.exception))
                self.assertIn("README.md:", str(caught.exception))
                self.assertIn("line=2", str(caught.exception))
        self.put("README.md", b"Use C:/path/example as a generic placeholder.\n")
        producer.plan(self.source)

    def test_new_python_script_is_scanned_without_exemptions(self):
        self.put("scripts/extra.py", ("token = '" + "sk" + "-" + "x" * 32 + "'\n").encode())
        with self.assertRaisesRegex(producer.Refusal, "provider-token"):
            producer.plan(self.source)

    def test_hardlink_refused(self):
        original = self.source / "src/lib.rs"
        hard = self.source / "src/hard.rs"
        try:
            os.link(original, hard)
        except OSError:
            self.skipTest("filesystem does not support hard links")
        with self.assertRaisesRegex(producer.Refusal, "single-link"):
            producer.plan(self.source)
    def test_symlink_refused(self):
        original = self.source / "src/lib.rs"
        link = self.source / "src/link.rs"
        try:
            link.symlink_to(original)
        except OSError:
            self.skipTest("host denied synthetic symlink creation")
        with self.assertRaisesRegex(producer.Refusal, "link-or-reparse"):
            producer.plan(self.source)

    def test_case_collision_in_directory_stream_is_refused(self):
        self.put("src/a.rs", b"a\n")
        self.put("src/b.rs", b"b\n")
        real_scandir = os.scandir

        class CaseEntry:
            def __init__(self, entry):
                self.name = "A.rs"
                self.path = entry.path

            def is_dir(self, follow_symlinks=False):
                return False

        def with_collision(path):
            entries = list(real_scandir(path))
            if Path(path).name == "src":
                return [CaseEntry(e) if e.name == "b.rs" else e for e in entries]
            return entries

        # Simulate a case-sensitive directory on Windows too. Real source bytes
        # are still read through the production reader; only names are varied.
        with mock.patch.object(producer.os, "scandir", side_effect=with_collision):
            with self.assertRaisesRegex(producer.Refusal, "case-collision"):
                producer.plan(self.source)

    def test_path_and_bounds_denials(self):
        for name in ("../a", "a:b", "/a", "a\\b", "a/CON.rs", "a/trailing. ", "out/x.rs"):
            with self.subTest(name=name), self.assertRaises(producer.Refusal):
                producer.safe_name(name)
        with mock.patch.object(producer, "MAX_FILE_BYTES", 8):
            with self.assertRaisesRegex(producer.Refusal, "file-byte-limit"):
                producer.plan(self.source)
        with mock.patch.object(producer, "MAX_FILES", 1):
            with self.assertRaisesRegex(producer.Refusal, "inventory-limit"):
                producer.plan(self.source)

    def test_payload_tampering_extra_file_and_line_endings_are_refused(self):
        dest, _ = self.staged()
        (dest / "src/lib.rs").write_bytes(b"changed\n")
        with self.assertRaisesRegex(producer.Refusal, "manifest-mismatch"):
            producer.verify(dest)
        other, _ = self.staged("extra")
        (other / "unlisted.txt").write_bytes(b"extra\n")
        with self.assertRaisesRegex(producer.Refusal, "unexpected-payload-file"):
            producer.verify(other)
        third, _ = self.staged("line-endings")
        (third / "Cargo.toml").write_bytes((third / "Cargo.toml").read_bytes().replace(b"\n", b"\r\n"))
        with self.assertRaisesRegex(producer.Refusal, "noncanonical-text"):
            producer.verify(third)

    def test_metadata_refresh_is_new_payload_and_does_not_change_ordinary_identity(self):
        first = producer.plan(self.source)
        metadata = self.base / "metadata"
        metadata.mkdir()
        rows = [dict(row, status="not_queried") for row in first["ordinaryFiles"]]
        value = dict(schema="amqic-vt-metadata/1", ordinaryEtag=first["ordinaryEtag"], files=rows)
        (metadata / "VIRUSTOTAL.json").write_bytes(producer.canonical(value))
        (metadata / "VIRUSTOTAL.md").write_bytes(b"Not queried; no malware verdict.\n")
        second = producer.plan(self.source, metadata)
        self.assertEqual(first["ordinaryEtag"], second["ordinaryEtag"])
        self.assertNotEqual(first["etag"], second["etag"])
        dest, verified = self.staged(metadata=metadata)
        self.assertEqual(verified, producer.verify(dest))
        sums = (dest / "SHA256SUMS").read_text()
        self.assertIn("  VIRUSTOTAL.json\n", sums)
        self.assertNotIn("  SHA256SUMS\n", sums)
        self.assertEqual(json.loads((dest / "FILES.json").read_bytes())["inputEtag"], first["ordinaryEtag"])
        value["files"][0]["sha256"] = "0" * 64
        (metadata / "VIRUSTOTAL.json").write_bytes(producer.canonical(value))
        with self.assertRaisesRegex(producer.Refusal, "vt-member-or-status-mismatch"):
            producer.plan(self.source, metadata)

    def test_metadata_rejects_missing_extra_and_duplicate_coverage(self):
        planned = producer.plan(self.source)
        metadata = self.base / "metadata"
        metadata.mkdir()
        (metadata / "VIRUSTOTAL.json").write_bytes(b"{}\n")
        with self.assertRaisesRegex(producer.Refusal, "exact-vt-pair"):
            producer.plan(self.source, metadata)
        (metadata / "VIRUSTOTAL.md").write_bytes(b"pending\n")
        rows = [dict(row, status="known") for row in planned["ordinaryFiles"]]
        rows[1] = dict(rows[0])
        (metadata / "VIRUSTOTAL.json").write_bytes(producer.canonical(dict(
            schema="amqic-vt-metadata/1", ordinaryEtag=planned["ordinaryEtag"], files=rows)))
        with self.assertRaisesRegex(producer.Refusal, "vt-member-or-status-mismatch"):
            producer.plan(self.source, metadata)
        (metadata / "foreign.txt").write_bytes(b"extra\n")
        with self.assertRaisesRegex(producer.Refusal, "exact-vt-pair"):
            producer.plan(self.source, metadata)

    def test_archive_readback_deterministic_bytes_and_stale_no_effect(self):
        dest, verified = self.staged()
        output = self.base / "source.zip"
        with self.assertRaisesRegex(producer.Refusal, "etag-mismatch"):
            producer.archive_payload(dest, output, producer.plan(self.source)["etag"])
        self.assertFalse(output.exists())
        outcome = producer.archive_payload(dest, output, verified["etag"])
        self.assertEqual(outcome["readback"], "exact-members-and-bytes")
        self.assertEqual(outcome["files"], verified["files"])
        other = self.base / "source-again.zip"
        producer.archive_payload(dest, other, verified["etag"])
        self.assertEqual(output.read_bytes(), other.read_bytes())
        before = output.read_bytes()
        with self.assertRaises(producer.Refusal):
            producer.archive_payload(dest, output, verified["etag"])
        self.assertEqual(output.read_bytes(), before)

    def test_archive_verifier_rejects_wrong_bytes_traversal_and_duplicates(self):
        expected = {"README.md": b"expected\n"}
        for label, entries in (("wrong", [("README.md", b"wrong\n")]),
                               ("traversal", [("../README.md", b"expected\n")]),
                               ("duplicate", [("README.md", b"expected\n"), ("README.md", b"expected\n")])):
            path = self.base / (label + ".zip")
            with contextlib.redirect_stderr(io.StringIO()):
                with zipfile.ZipFile(path, "w") as archive:
                    for name, body in entries:
                        archive.writestr(name, body)
            with self.subTest(label=label), self.assertRaises(producer.Refusal):
                producer.verify_archive(path, expected)

    def test_connected_cli_plan_stage_verify_archive(self):
        script = Path(producer.__file__).resolve()

        def run(*args):
            completed = subprocess.run([sys.executable, "-B", str(script), *map(str, args)],
                                       capture_output=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr.decode())
            return json.loads(completed.stdout)

        planned = run("plan", "--source", self.source)
        dest = self.base / "cli-payload"
        staged = run("stage", "--source", self.source, "--destination", dest,
                     "--expected-etag", planned["etag"])
        verified = run("verify", "--payload", dest)
        self.assertEqual(staged, verified)
        archived = run("archive", "--payload", dest, "--output", self.base / "cli.zip",
                       "--expected-etag", verified["etag"])
        self.assertEqual(archived["files"], verified["files"])


if __name__ == "__main__":
    unittest.main()
