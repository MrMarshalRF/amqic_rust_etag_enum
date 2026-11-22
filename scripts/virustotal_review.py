#!/usr/bin/env python3
"""Human-operated, fixed-origin VirusTotal review of a sealed source payload.

No key argument, environment lookup, stdin fallback or stored-secret broker.
Hash queries also disclose identifiers. Uploads require explicit terminal consent.
The output and append-only upload ledger must remain outside the public payload.
"""
import argparse
from collections import Counter
from datetime import datetime, timezone
import getpass
import json
import os
from pathlib import Path
import re
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import warnings

import prepare_repository as prep

API = "https://www.virustotal.com/api/v3"
MAX_RESPONSE = 4 * 1024 * 1024
CATEGORIES = {"malicious", "suspicious", "undetected", "harmless", "timeout",
              "confirmed-timeout", "failure", "type-unsupported"}


class SafeStop(Exception):
    pass


def now():
    return datetime.now(timezone.utc).isoformat()


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, key, clock=time.monotonic, sleep=time.sleep):
        self._key = key
        self._clock, self._sleep, self._last = clock, sleep, None
        self._opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}), NoRedirect(),
            urllib.request.HTTPSHandler(context=ssl.create_default_context()))

    def request(self, path, body=None, content_type=None):
        # Paths are constructed here, never taken from a response URL or caller.
        if not (re.fullmatch(r"/files/[0-9a-f]{64}", path)
                or path == "/files" or re.fullmatch(r"/analyses/[A-Za-z0-9%._~-]+", path)):
            raise SafeStop("invalid fixed API operation")
        if self._last is not None:
            delay = 15.1 - (self._clock() - self._last)
            if delay > 0:
                self._sleep(delay)
        self._last = self._clock()
        headers = {"x-apikey": self._key, "Accept": "application/json"}
        if content_type:
            headers["Content-Type"] = content_type
        request = urllib.request.Request(API + path, data=body, headers=headers,
                                         method="POST" if body is not None else "GET")
        try:
            with self._opener.open(request, timeout=30) as response:
                raw = response.read(MAX_RESPONSE + 1)
                if len(raw) > MAX_RESPONSE:
                    return 0, None
                return response.status, json.loads(raw)
        except urllib.error.HTTPError as error:
            # Never expose response bodies, headers, request objects or key-bearing traces.
            return error.code, None
        except Exception:
            return 0, None

    def lookup(self, sha):
        return self.request("/files/" + sha)

    def upload(self, name, data):
        boundary = "amqic" + uuid.uuid4().hex
        safe = Path(name).name
        if not re.fullmatch(r"[A-Za-z0-9_.-]+", safe):
            raise SafeStop("unsupported upload filename")
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="{safe}"\r\n'
                'Content-Type: application/octet-stream\r\n\r\n').encode() + data
        body += f"\r\n--{boundary}--\r\n".encode()
        return self.request("/files", body, "multipart/form-data; boundary=" + boundary)

    def analysis(self, identifier):
        if not isinstance(identifier, str) or not 1 <= len(identifier) <= 512:
            return 0, None
        return self.request("/analyses/" + urllib.parse.quote(identifier, safe=""))


class Ledger:
    """One operator owns a locked ledger; unknown upload intents never POST again."""
    def __init__(self, path):
        self.path, self.state, self.lock = Path(path), {}, None

    def __enter__(self):
        lock_path = self.path.with_suffix(self.path.suffix + ".lock")
        prep.checked_absolute(lock_path, new=not lock_path.exists())
        self.lock = lock_path.open("a+b")
        if self.lock.tell() == 0:
            self.lock.write(b"1"); self.lock.flush()
        self.lock.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(self.lock.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if self.path.exists():
                if self.path.stat().st_size > MAX_RESPONSE:
                    raise SafeStop("upload ledger bound")
                for line in self.path.read_text(encoding="utf-8").splitlines():
                    row = json.loads(line)
                    if (row.get("schema") != "amqic-vt-upload/1"
                            or not prep.HEX.fullmatch(str(row.get("sha256", "")))
                            or row.get("event") not in {"intent", "confirmed", "unknown"}):
                        raise SafeStop("invalid upload ledger; preserve it")
                    self.state[row["sha256"]] = row
        except Exception:
            self.lock.close(); self.lock = None
            raise SafeStop("upload ledger unavailable or invalid; preserve it") from None
        return self

    def append(self, sha, event, ordinary_etag, analysis_id=None):
        row = dict(schema="amqic-vt-upload/1", sha256=sha, event=event,
                   ordinaryEtag=ordinary_etag, at=now())
        if analysis_id is not None:
            row["analysis_id"] = analysis_id
        with self.path.open("ab") as stream:
            stream.write(prep.canonical(row)); stream.flush(); os.fsync(stream.fileno())
        self.state[sha] = row

    def __exit__(self, *_):
        if self.lock:
            self.lock.close()  # OS lock is released, retained file is not deleted.


def outside(path, root):
    value = Path(os.path.abspath(path))
    if value == root or root in value.parents:
        raise SafeStop("private output/ledger must be outside the payload")
    prep.checked_absolute(value, new=not value.exists())
    if not value.parent.is_dir():
        raise SafeStop("private output parent must exist")
    return value


def freeze(payload, expected):
    root = prep.checked_absolute(payload)
    verified = prep.verify(root)
    prep.check_expected(expected, verified["etag"])
    manifest = json.loads(prep.read_bytes(root / "FILES.json"))
    return root, manifest, verified


def observations(item, response):
    try:
        attrs = response["data"]["attributes"]
        if attrs["sha256"] != item["sha256"] or type(attrs["size"]) is not int or attrs["size"] != item["size"]:
            raise SafeStop("provider file identity mismatch")
        engines = attrs.get("last_analysis_results")
        date = attrs.get("last_analysis_date")
        if not isinstance(engines, dict) or not engines or type(date) is not int:
            return dict(item, status="unsupported", retrieved_at=now(), reason="no completed engine observation")
        if len(engines) > 256:
            raise SafeStop("engine observation bound")
        clean = {}
        counts = Counter()
        for name, row in engines.items():
            if not isinstance(name, str) or not isinstance(row, dict):
                raise SafeStop("invalid engine observation")
            category = row.get("category")
            if category not in CATEGORIES:
                category = "other"
            counts[category] += 1
            clean[name[:128]] = {key: value[:512] if isinstance(value, str) else None
                                for key, value in row.items() if key in
                                {"engine_name", "engine_version", "engine_update", "method", "result"}}
            clean[name[:128]]["category"] = category
        return dict(item, status="known", analysis_date=date, retrieved_at=now(),
                    engine_counts=dict(counts), engines=clean,
                    no_detections_observed=not (counts["malicious"] or counts["suspicious"]),
                    all_supported_engines_completed_without_error=not any(counts[k] for k in
                        ("failure", "timeout", "confirmed-timeout", "other")))
    except SafeStop:
        raise
    except Exception:
        raise SafeStop("invalid provider file report") from None


def markdown(report):
    lines = ["# VirusTotal file observations", "", "Exact ordinary source files only. Generated metadata is not recursively self-scanned.",
             "Undetected is not proof of safety. Unsupported/failure/timeout results remain limits.", "",
             "| File | SHA-256 | Status | Malicious | Suspicious | Unsupported | Failure/timeouts |", "|---|---|---|---:|---:|---:|---:|"]
    for row in report["files"]:
        counts = row.get("engine_counts", {})
        failures = sum(counts.get(k, 0) for k in ("failure", "timeout", "confirmed-timeout", "other"))
        values = [str(counts.get(k, 0)) for k in ("malicious", "suspicious", "type-unsupported")] + [str(failures)] if row["status"] == "known" else ["—"] * 4
        lines.append(f'| {row["path"]} | [{row["sha256"]}](https://www.virustotal.com/gui/file/{row["sha256"]}) | {row["status"]} | ' + " | ".join(values) + " |")
    return "\n".join(lines) + "\n"


def save(report, output):
    companion = output.with_suffix(".md")
    prep.checked_absolute(output, new=not output.exists())
    prep.checked_absolute(companion, new=not companion.exists())
    if companion.exists() and not output.exists():
        raise SafeStop("unowned companion report; preserve it")
    if output.exists():
        previous = json.loads(prep.read_bytes(output))
        if (previous.get("schema") != "amqic-vt-metadata/1"
                or previous.get("ordinaryEtag") != report["ordinaryEtag"]
                or [{k: r.get(k) for k in ("path", "size", "sha256")} for r in previous.get("files", [])]
                != [{k: r[k] for k in ("path", "size", "sha256")} for r in report["files"]]):
            raise SafeStop("progress output belongs to another operation; preserve it")
        if companion.exists() and prep.read_bytes(companion) != markdown(previous).encode("utf-8"):
            raise SafeStop("companion mismatch; preserve reports and reuse the same upload ledger in a new report directory")
    body = bytearray()
    for chunk in json.JSONEncoder(ensure_ascii=True, sort_keys=True, separators=(",", ":")).iterencode(report):
        encoded = chunk.encode("utf-8")
        if len(body) + len(encoded) + 1 > MAX_RESPONSE:
            raise SafeStop("report byte limit; preserve upload ledger")
        body.extend(encoded)
    body.extend(b"\n")
    for path, data in ((output, bytes(body)),
                       (output.with_suffix(".md"), markdown(report).encode("utf-8"))):
        temp = path.with_suffix(path.suffix + "." + uuid.uuid4().hex + ".partial")
        with temp.open("xb") as stream:
            stream.write(data); stream.flush(); os.fsync(stream.fileno())
        os.replace(temp, path)  # only this operation's private progress output


def scan(root, manifest, client, ledger, allow_upload, output, max_polls=4):
    report = dict(schema="amqic-vt-metadata/1", ordinaryEtag=manifest["ordinaryEtag"],
                  generated_at=now(), mode="upload-approved" if allow_upload else "hash-only",
                  complete=False, files=[dict(row, status="not_queried") for row in manifest["files"]])
    save(report, output)
    for index, item in enumerate(manifest["files"]):
        stop_status = None
        report["files"][index] = dict(item, status="unknown", reason="lookup outcome pending", retrieved_at=now())
        save(report, output)
        row = dict(item, status="not_found", retrieved_at=now())
        status, data = client.lookup(item["sha256"])
        if status == 200:
            row = observations(item, data)
        elif status == 404:
            prior = ledger.state.get(item["sha256"])
            identifier = prior.get("analysis_id") if prior else None
            if allow_upload and prior is None:
                body = prep.read_bytes(root / item["path"])
                if len(body) != item["size"] or prep.digest(body) != item["sha256"]:
                    raise SafeStop("source changed before upload")
                ledger.append(item["sha256"], "intent", manifest["ordinaryEtag"])
                report["files"][index] = dict(item, status="unknown", reason="synced upload intent; outcome pending", retrieved_at=now())
                save(report, output)
                try:
                    code, response = client.upload(item["path"], body)
                    if code in (401, 403, 429):
                        stop_status = code
                    identifier = response["data"]["id"] if code in (200, 201) else None
                    if not isinstance(identifier, str) or not 1 <= len(identifier) <= 512:
                        identifier = None
                    ledger.append(item["sha256"], "confirmed" if identifier else "unknown",
                                  manifest["ordinaryEtag"], identifier)
                except Exception:
                    # The synced intent already prevents a blind upload retry.
                    identifier = None
            if prior or allow_upload:
                row = dict(item, status="unknown", retrieved_at=now(), reason="upload outcome pending; no blind replay")
            if identifier:
                for _ in range(max_polls):
                    code, analysis = client.analysis(identifier)
                    if code in (401, 403, 429):
                        stop_status = code
                    if code != 200:
                        break
                    if not isinstance(analysis, dict):
                        break
                    if analysis.get("data", {}).get("attributes", {}).get("status") == "completed":
                        code, response = client.lookup(item["sha256"])
                        if code in (401, 403, 429):
                            stop_status = code
                        if code == 200:
                            row = observations(item, response)
                        break
        else:
            row = dict(item, status="failed", retrieved_at=now(), http_status=status)
        report["files"][index] = row
        if stop_status is not None:
            row["http_status"] = stop_status
        report["generated_at"] = now()
        report["complete"] = all(r["status"] == "known" for r in report["files"])
        save(report, output)
        if status in (401, 403, 429) or stop_status is not None:
            break
    return report


def hidden_key():
    if not sys.stdin.isatty() or not sys.stderr.isatty():
        raise SafeStop("human terminal required; no stdin/environment/argument key fallback")
    with warnings.catch_warnings():
        warnings.simplefilter("error", getpass.GetPassWarning)
        try:
            key = getpass.getpass("VirusTotal API key (hidden; never paste in chat): ")
        except Exception:
            raise SafeStop("protected terminal input unavailable") from None
    if not key or len(key) > 512 or "\r" in key or "\n" in key:
        raise SafeStop("invalid hidden input")
    return key


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", required=True)
    parser.add_argument("--expected-etag", required=True)
    parser.add_argument("--output", required=True, help="private VIRUSTOTAL.json outside payload")
    parser.add_argument("--ledger", required=True, help="retained private upload JSONL outside metadata pair")
    parser.add_argument("--upload-approved", action="store_true")
    parser.add_argument("--max-polls", type=int, choices=range(1, 21), default=4)
    args = parser.parse_args(argv)
    root, manifest, _ = freeze(args.payload, args.expected_etag)
    if Path(__file__).resolve() != (root / "scripts/virustotal_review.py").resolve():
        raise SafeStop("run the exact reviewed scanner inside the sealed payload")
    output, ledger_path = outside(args.output, root), outside(args.ledger, root)
    outside(output.with_suffix(".md"), root)
    if output.name != "VIRUSTOTAL.json" or output.parent == ledger_path.parent:
        raise SafeStop("use a dedicated metadata pair directory and a separate retained ledger")
    if output.exists():
        previous = json.loads(prep.read_bytes(output))
        if (previous.get("schema") != "amqic-vt-metadata/1"
                or previous.get("ordinaryEtag") != manifest["ordinaryEtag"]
                or [{k: row.get(k) for k in ("path", "size", "sha256")} for row in previous.get("files", [])] != manifest["files"]):
            raise SafeStop("existing report belongs to another snapshot; preserve it")
    if not sys.stdin.isatty() or not sys.stderr.isatty():
        raise SafeStop("human terminal required before network access")
    print(f'Reviewing {len(manifest["files"])} ordinary files; {manifest["ordinaryEtag"]}', file=sys.stderr)
    print("Hashes leave this machine. Standard uploads share full contents with VirusTotal/partners.", file=sys.stderr)
    if args.upload_approved:
        phrase = "UPLOAD " + manifest["ordinaryEtag"][-13:-1]
        if input("Do not enter a key here. Type " + phrase + " to approve this exact batch: ") != phrase:
            raise SafeStop("upload disclosure not confirmed")
    with Ledger(ledger_path) as ledger:
        key = hidden_key()
        report = scan(root, manifest, Client(key), ledger, args.upload_approved, output, args.max_polls)
    print(json.dumps({"complete": report["complete"], "counts": dict(Counter(r["status"] for r in report["files"]))}))
    return 0 if report["complete"] else 2


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (SafeStop, prep.Refusal):
        print("VirusTotal operation refused or incomplete; preserve private reports/ledger and review prerequisites.", file=sys.stderr)
        raise SystemExit(1)
    except KeyboardInterrupt:
        print("Cancelled; preserve the original upload ledger and private progress files.", file=sys.stderr)
        raise SystemExit(130)
    except Exception:
        print("VirusTotal operation failed; no credential-bearing diagnostics are emitted.", file=sys.stderr)
        raise SystemExit(1)
