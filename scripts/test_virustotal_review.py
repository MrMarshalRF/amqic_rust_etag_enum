#!/usr/bin/env python3
"""Synthetic tests. No live service, real key or human input is used."""
import getpass
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import virustotal_review as vt


class FakeClient:
    def __init__(self, lookups, upload=(0, None), analysis=(0, None)):
        self.responses = list(lookups)
        self.upload_response, self.analysis_response = upload, analysis
        self.lookup_calls = self.upload_calls = self.analysis_calls = 0

    def lookup(self, _):
        self.lookup_calls += 1
        return self.responses.pop(0)

    def upload(self, *_):
        self.upload_calls += 1
        return self.upload_response

    def analysis(self, *_):
        self.analysis_calls += 1
        return self.analysis_response


class ReviewTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.payload = self.base / "payload"; self.payload.mkdir()
        self.private = self.base / "metadata"; self.private.mkdir()
        self.output = self.private / "VIRUSTOTAL.json"
        self.ledger_path = self.base / "operations.jsonl"
        data = b"synthetic source\n"
        (self.payload / "README.md").write_bytes(data)
        self.item = dict(path="README.md", size=len(data), sha256=vt.prep.digest(data))
        self.manifest = dict(ordinaryEtag='"publication-ordinary-v1-' + "0" * 64 + '"', files=[self.item])

    def known(self, **categories):
        return {"data": {"attributes": {"sha256": self.item["sha256"], "size": self.item["size"],
            "last_analysis_date": 1, "last_analysis_results": {
                name: {"category": category, "engine_name": name, "result": None}
                for name, category in categories.items()}}}}

    def run_scan(self, client, upload=False, manifest=None):
        with vt.Ledger(self.ledger_path) as ledger:
            return vt.scan(self.payload, manifest or self.manifest, client, ledger, upload, self.output, 1)

    def test_hidden_input_requires_real_terminal(self):
        with patch.object(vt.sys.stdin, "isatty", return_value=False), patch.object(vt.getpass, "getpass") as prompt:
            with self.assertRaises(vt.SafeStop): vt.hidden_key()
            prompt.assert_not_called()

    def test_getpass_warning_never_falls_back_to_echo(self):
        with patch.object(vt.sys.stdin, "isatty", return_value=True), patch.object(vt.sys.stderr, "isatty", return_value=True), \
             patch.object(vt.getpass, "getpass", side_effect=getpass.GetPassWarning("fixture")):
            with self.assertRaises(vt.SafeStop): vt.hidden_key()

    def test_fixed_origin_rejects_arbitrary_operation(self):
        client = vt.Client("unit" + "-fixture")
        with self.assertRaises(vt.SafeStop): client.request("https://example.invalid/collect")
        with self.assertRaises(vt.SafeStop): client.request("/files/../collect")
        self.assertIsNone(vt.NoRedirect().redirect_request(None, None, 302, "", {}, "https://example.invalid"))

    def test_hash_only_not_found_never_uploads(self):
        client = FakeClient([(404, None)])
        result = self.run_scan(client)
        self.assertEqual(result["files"][0]["status"], "not_found")
        self.assertFalse(result["complete"]); self.assertEqual(client.upload_calls, 0)
        self.assertIn("| — | — | — | — |", self.output.with_suffix(".md").read_text(encoding="utf-8"))

    def test_known_retains_unsupported_failure_timeout(self):
        client = FakeClient([(200, self.known(a="undetected", b="type-unsupported", c="failure", d="timeout"))])
        row = self.run_scan(client)["files"][0]
        self.assertEqual(row["status"], "known")
        self.assertEqual(row["engine_counts"]["type-unsupported"], 1)
        self.assertFalse(row["all_supported_engines_completed_without_error"])
        self.assertTrue(row["no_detections_observed"])

    def test_identity_mismatch_is_not_a_known_report(self):
        wrong = self.known(a="undetected"); wrong["data"]["attributes"]["sha256"] = "1" * 64
        with self.assertRaises(vt.SafeStop): self.run_scan(FakeClient([(200, wrong)]))
        self.assertEqual(json.loads(self.output.read_text())["files"][0]["status"], "unknown")

    def test_unknown_upload_is_not_repeated_after_reopen(self):
        first = FakeClient([(404, None)], upload=(0, None))
        self.assertEqual(self.run_scan(first, True)["files"][0]["status"], "unknown")
        second = FakeClient([(404, None)])
        self.run_scan(second, True)
        self.assertEqual((first.upload_calls, second.upload_calls), (1, 0))
        rows = [json.loads(line) for line in self.ledger_path.read_text().splitlines()]
        self.assertEqual(rows[0]["event"], "intent")

    def test_intent_failure_prevents_upload(self):
        client = FakeClient([(404, None)])
        with vt.Ledger(self.ledger_path) as ledger, patch.object(ledger, "append", side_effect=OSError("fixture")):
            with self.assertRaises(OSError): vt.scan(self.payload, self.manifest, client, ledger, True, self.output, 1)
        self.assertEqual(client.upload_calls, 0)

    def test_upload_quota_stops_without_next_file(self):
        two = dict(self.manifest, files=[self.item, dict(self.item, path="other.md")])
        client = FakeClient([(404, None)], upload=(429, None))
        result = self.run_scan(client, True, two)
        self.assertEqual((client.lookup_calls, client.upload_calls), (1, 1))
        self.assertEqual(result["files"][0]["http_status"], 429)
        self.assertEqual(result["files"][1]["status"], "not_queried")

    def test_poll_quota_stops_and_preserves_confirmed_identifier(self):
        two = dict(self.manifest, files=[self.item, dict(self.item, path="other.md")])
        with vt.Ledger(self.ledger_path) as ledger:
            ledger.append(self.item["sha256"], "intent", self.manifest["ordinaryEtag"])
            ledger.append(self.item["sha256"], "confirmed", self.manifest["ordinaryEtag"], "fixture-analysis")
        client = FakeClient([(404, None)], analysis=(429, None))
        result = self.run_scan(client, True, two)
        self.assertEqual((client.lookup_calls, client.upload_calls, client.analysis_calls), (1, 0, 1))
        self.assertEqual(result["files"][0]["http_status"], 429)
        self.assertEqual(result["files"][1]["status"], "not_queried")

    def test_completed_upload_matches_current_file_and_never_reuploads(self):
        client = FakeClient([(404, None), (200, self.known(a="undetected"))],
            upload=(200, {"data": {"id": "fixture-analysis"}}),
            analysis=(200, {"data": {"attributes": {"status": "completed"}}}))
        result = self.run_scan(client, True)
        self.assertTrue(result["complete"])
        self.assertEqual((client.upload_calls, client.analysis_calls), (1, 1))
        repeated = FakeClient([(200, self.known(a="undetected"))])
        self.run_scan(repeated, True)
        self.assertEqual(repeated.upload_calls, 0)

    def test_final_report_quota_stops_after_completed_analysis(self):
        two = dict(self.manifest, files=[self.item, dict(self.item, path="other.md")])
        client = FakeClient([(404, None), (429, None)],
            upload=(200, {"data": {"id": "fixture-analysis"}}),
            analysis=(200, {"data": {"attributes": {"status": "completed"}}}))
        result = self.run_scan(client, True, two)
        self.assertEqual((client.lookup_calls, client.upload_calls, client.analysis_calls), (2, 1, 1))
        self.assertEqual(result["files"][0]["http_status"], 429)
        self.assertEqual(result["files"][1]["status"], "not_queried")

    def test_actual_transport_request_and_exception_do_not_expose_key(self):
        key = "fixture" + "-not-real-credential"
        ticks = [0.0]
        sleeps = []
        def sleep(delay): sleeps.append(delay); ticks[0] += delay
        client = vt.Client(key, clock=lambda: ticks[0], sleep=sleep)
        requests = []
        class Reply:
            status = 200
            def __enter__(self): return self
            def __exit__(self, *_): pass
            def read(self, _): return b'{"data":{"id":"fixture-analysis"}}'
        class Opener:
            def open(self, request, timeout): requests.append((request, timeout)); return Reply()
        client._opener = Opener()
        client.lookup(self.item["sha256"])
        client.upload("README.md", b"public fixture")
        self.assertEqual(len(requests), 2)
        for request, timeout in requests:
            self.assertTrue(request.full_url.startswith(vt.API + "/files"))
            self.assertNotIn(key, request.full_url)
            self.assertNotIn(key.encode(), request.data or b"")
            self.assertEqual(request.get_header("X-apikey"), key)
            self.assertEqual(timeout, 30)
        self.assertEqual(requests[0][0].get_method(), "GET")
        self.assertEqual(requests[1][0].get_method(), "POST")
        self.assertEqual(sleeps, [15.1])
        class TooLarge(Reply):
            def read(self, limit): return b"x" * limit
        class LargeOpener:
            def open(self, *_args, **_kwargs): return TooLarge()
        client._opener = LargeOpener()
        self.assertEqual(client.lookup(self.item["sha256"]), (0, None))
        class Broken:
            def open(self, *_args, **_kwargs): raise RuntimeError(key)
        client._opener = Broken()
        import contextlib
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            self.assertEqual(client.lookup(self.item["sha256"]), (0, None))
        self.assertNotIn(key, output.getvalue())

    def test_unowned_companion_and_stale_temp_are_preserved(self):
        companion = self.output.with_suffix(".md"); companion.write_text("foreign fixture")
        with self.assertRaises(vt.SafeStop): self.run_scan(FakeClient([(404, None)]))
        self.assertEqual(companion.read_text(), "foreign fixture")
        other = self.private / "new"; other.mkdir(); self.output = other / "VIRUSTOTAL.json"
        stale = self.output.with_suffix(".json.partial"); stale.write_text("old fixture")
        self.run_scan(FakeClient([(404, None)]))
        self.assertEqual(stale.read_text(), "old fixture")

    def test_sidecar_hardlink_is_refused(self):
        target = self.base / "unrelated.txt"; target.write_text("keep fixture")
        import os
        try: os.link(target, self.ledger_path.with_suffix(".jsonl.lock"))
        except OSError as error: self.skipTest("hardlink creation unavailable: " + type(error).__name__)
        with self.assertRaises((vt.SafeStop, vt.prep.Refusal)):
            with vt.Ledger(self.ledger_path): pass
        self.assertEqual(target.read_text(), "keep fixture")

    def test_outputs_cannot_be_inside_payload(self):
        with self.assertRaises(vt.SafeStop): vt.outside(self.payload / "report.json", self.payload)


if __name__ == "__main__":
    unittest.main()
