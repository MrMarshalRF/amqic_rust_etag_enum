#!/usr/bin/env python3
"""Prepare an offline, bounded, source-only publication in a NEW directory.

All selected text becomes UTF-8 without BOM and LF before hashing. Input ETags
bind original bytes as well as normalized bytes; result ETags describe the
returned representation independently. No network, commands, environment or
credential stores are accessed. This is a release aid, not a secret detector
with universal coverage or protection against hostile concurrent OS actors.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys
import zipfile

SCHEMA = "amqic-source-publication/1"
MAX_FILES = 512
MAX_FILE_BYTES = 4 * 1024 * 1024
MAX_TOTAL_BYTES = 32 * 1024 * 1024
MAX_ENTRIES = 2048
ROOT_FILES = frozenset(("Cargo.toml", "Cargo.lock", "README.md", "LICENSE",
    "COMMERCIAL-LICENSE.md", "SECURITY.md", "THIRD-PARTY-NOTICES.md",
    "CONTRIBUTING.md", "CHANGELOG.md", "wallets.txt", ".gitignore", ".gitattributes"))
DOC_FILES = frozenset(("PROTOCOL.md", "DURABILITY.md", "COMPATIBILITY.md",
    "VALIDATION.md", "PUBLICATION.md", "ENUMS.md", "USAGE.md"))
GENERATED = frozenset(("FILES.json", "SHA256SUMS", "VIRUSTOTAL.json", "VIRUSTOTAL.md"))
VT_FILES = frozenset(("VIRUSTOTAL.json", "VIRUSTOTAL.md"))
DENIED_PARTS = frozenset(("target", "out", ".git", ".codex", ".agents",
    ".claude", ".vscode", "private", "local", "secrets",
    "logs", "backups", "temp", "__pycache__"))
DENIED_SUFFIXES = (".pem", ".key", ".p12", ".db", ".sqlite", ".sqlite3",
    ".log", ".exe", ".dll", ".zip", ".bak", ".tmp")
RESERVED = re.compile(r"^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)", re.I)
HEX = re.compile(r"^[0-9a-f]{64}$")
STRONG = re.compile(r'^"[a-z0-9-]+-[0-9a-f]{64}"$')


class Refusal(Exception):
    """Safe diagnostic containing a path, rule and optional line only."""


def refuse(path, rule, line=None):
    suffix = "" if line is None else ":line=" + str(line)
    raise Refusal(str(path) + ":" + rule + suffix)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return (json.dumps(value, ensure_ascii=True, sort_keys=True,
                       separators=(",", ":")) + "\n").encode("utf-8")


def tag(namespace, value):
    return '"' + namespace + "-" + digest(canonical(value)) + '"'


def result(operation, input_etag, **fields):
    value = dict(schema=SCHEMA, operation=operation, inputEtag=input_etag, **fields)
    value["etag"] = tag("publication-result-v1", value)
    return value


def check_expected(expected, actual):
    if not isinstance(expected, str) or not STRONG.fullmatch(expected):
        refuse("<precondition>", "strong-etag-required")
    if expected != actual:
        refuse("<precondition>", "etag-mismatch")


def private_hidden_component(parts):
    public_root_names = {".github", ".gitignore", ".gitattributes"}
    return any(part.startswith(".") and not (i == 0 and part in public_root_names)
               for i, part in enumerate(parts))


def safe_name(name):
    if not isinstance(name, str) or not name or len(name) > 240:
        refuse("<path>", "invalid-path")
    parts = name.split("/")
    if ("\\" in name or ":" in name or name.startswith("/")
            or any(p in ("", ".", "..") or p.endswith((".", " "))
                   or RESERVED.match(p) or any(ord(c) < 32 for c in p)
                   or any(c in p for c in '<>"|?*') for p in parts)):
        refuse("<path>", "invalid-path")
    if private_hidden_component(parts) or any(p.casefold() in DENIED_PARTS or p.casefold().startswith(".env")
           or p.casefold().endswith(DENIED_SUFFIXES) for p in parts):
        refuse(name, "private-path")
    return name


def allowed(name):
    p = PurePosixPath(name)
    if name in ROOT_FILES:
        return True
    if len(p.parts) == 2 and p.parts[0] == "docs":
        return p.name in DOC_FILES
    if p.parts[0] in ("src", "examples", "tests"):
        return len(p.parts) >= 2 and p.suffix == ".rs"
    if p.parts[0] == "fixtures":
        return len(p.parts) >= 2 and p.suffix == ".json"
    if len(p.parts) == 2 and p.parts[0] == "scripts":
        return p.suffix == ".py"
    if len(p.parts) == 3 and p.parts[:2] == (".github", "workflows"):
        return p.suffix in (".yml", ".yaml")
    if len(p.parts) == 2 and p.parts[0] == "integrations":
        return p.name.endswith(".example.toml") or p.suffix == ".json"
    return False


def check_node(path, directory=False):
    s = path.lstat()
    if stat.S_ISLNK(s.st_mode) or getattr(s, "st_file_attributes", 0) & 0x400:
        refuse(path.name, "link-or-reparse")
    if directory:
        if not stat.S_ISDIR(s.st_mode):
            refuse(path.name, "not-directory")
    elif not stat.S_ISREG(s.st_mode) or s.st_nlink != 1:
        refuse(path.name, "not-ordinary-single-link-file")
    return s


def checked_absolute(path, new=False):
    p = Path(os.path.abspath(path))
    # Check existing ancestors without resolving away a link first.
    chain = list(reversed(p.parents)) + [p]
    for i, member in enumerate(chain):
        if member.exists() or member.is_symlink():
            check_node(member, directory=(i < len(chain) - 1 or member.is_dir()))
        elif not (new and i == len(chain) - 1):
            refuse("<root>", "missing-parent-or-root")
    return p


def identity(s):
    # Windows Python can expose creation time in lstat.st_ctime but change time
    # in fstat.st_ctime. Device/inode/size/mtime are comparable across both APIs.
    return (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns)


def read_bytes(path):
    before = check_node(path)
    if before.st_size > MAX_FILE_BYTES:
        refuse(path.name, "file-byte-limit")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(path, flags)
    with os.fdopen(fd, "rb") as stream:
        opened = os.fstat(stream.fileno())
        if identity(before) != identity(opened) or opened.st_nlink != 1:
            refuse(path.name, "changed-during-read")
        data = stream.read(MAX_FILE_BYTES + 1)
        after = os.fstat(stream.fileno())
    if len(data) > MAX_FILE_BYTES or identity(opened) != identity(after):
        refuse(path.name, "changed-during-read")
    if identity(before) != identity(check_node(path)):
        refuse(path.name, "changed-during-read")
    return data


def normalize(data, name):
    try:
        text = data.decode("utf-8-sig")
    except UnicodeError:
        refuse(name, "not-utf8-text")
    if "\0" in text:
        refuse(name, "binary-text")
    return text.replace("\r\n", "\n").replace("\r", "\n").encode("utf-8")


# Patterns are deliberately assembled: the producer and its tests are scanned
# under the same rules as all other selected source. Findings never echo values.
CONTENT_RULES = (
    ("private-machine-path", re.compile(r"(?i)(?:[a-z]:[\\/](?!path(?:[\\/]|$))[A-Za-z0-9_.-]+|/" + r"home/[^/\s]+/|/" + r"Users/[^/\s]+/)")),
    ("agent-uuid", re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", re.I)),
    ("private-key", re.compile("-----BEGIN " + r"(?:[A-Z]+ )?PRIVATE KEY-----")),
    ("provider-token", re.compile(r"\b(?:" + "gh" + r"[pousr]_[A-Za-z0-9]{20,}|github_" + r"pat_[A-Za-z0-9_]{20,}|sk" + r"-[A-Za-z0-9_-]{20,}|AK" + r"IA[A-Z0-9]{16})\b")),
    ("assigned-secret", re.compile(r"(?i)\b(?:api[_-]?key|secret|password|access[_-]?token)\s*[:=]\s*[\"']?[A-Za-z0-9_+/=-]{20,}")),
    ("agent-receipt", re.compile(r"(?i)(?:receiptId|invocationId|readerId)\s*[\"']?\s*[:=]\s*[\"']?(?:sha256:|[0-9a-f]{8}-)")),
)


def scan(data, name):
    text = data.decode("utf-8")
    for rule, pattern in CONTENT_RULES:
        match = pattern.search(text)
        if match:
            refuse(name, rule, text.count("\n", 0, match.start()) + 1)


def record(name, data):
    return dict(path=name, size=len(data), sha256=digest(data))


def inventory(files):
    return [record(name, data) for name, data in sorted(files.items())]


def limits(files):
    if len(files) > MAX_FILES or sum(map(len, files.values())) > MAX_TOTAL_BYTES:
        refuse("<inventory>", "inventory-limit")


def collect(root, payload=False):
    root = checked_absolute(root)
    check_node(root, directory=True)
    files, raw_records, seen = {}, [], set()
    count = 0
    roots = {"src", "examples", "tests", "fixtures", "scripts", "docs", "integrations", ".github"}

    def walk(directory, prefix=""):
        nonlocal count
        for item in sorted(os.scandir(directory), key=lambda x: x.name):
            count += 1
            if count > MAX_ENTRIES:
                refuse("<inventory>", "entry-limit")
            name = prefix + item.name
            # Source discovery never opens unknown/private files or descends into
            # unselected trees. Payload validation instead rejects every extra.
            if not payload:
                parts = name.split("/")
                if private_hidden_component(parts) or any(p.casefold() in DENIED_PARTS or p.casefold().startswith(".env") for p in parts):
                    continue
                if not prefix and item.name not in roots and name not in ROOT_FILES:
                    continue
                if prefix == "docs/" and item.name not in DOC_FILES:
                    continue
                if prefix == ".github/" and item.name != "workflows":
                    continue
            safe_name(name)
            folded = name.casefold()
            if folded in seen:
                refuse(name, "case-collision")
            seen.add(folded)
            if item.is_dir(follow_symlinks=False):
                check_node(Path(item.path), directory=True)
                walk(Path(item.path), name + "/")
            else:
                if not allowed(name) and not (payload and name in GENERATED):
                    if payload:
                        refuse(name, "unexpected-payload-file")
                    continue
                raw = read_bytes(Path(item.path))
                data = normalize(raw, name)
                if payload and data != raw:
                    refuse(name, "noncanonical-text")
                scan(data, name)
                files[name] = data
                raw_records.append(record(name, raw))
                limits(files)
    walk(root)
    return files, sorted(raw_records, key=lambda x: x["path"])


def ordinary_descriptor(files):
    return dict(schema=SCHEMA, scope="allowlisted-source", normalization="utf8-no-bom-lf",
                limits=dict(files=MAX_FILES, fileBytes=MAX_FILE_BYTES, totalBytes=MAX_TOTAL_BYTES),
                files=inventory(files))


def vt_metadata(directory, ordinary):
    if directory is None:
        return {}
    root = checked_absolute(directory)
    check_node(root, directory=True)
    names = {entry.name for entry in os.scandir(root)}
    if names != VT_FILES:
        refuse("<metadata>", "exact-vt-pair-required")
    files = {}
    for name in sorted(VT_FILES):
        raw = read_bytes(root / name)
        data = normalize(raw, name)
        scan(data, name)
        files[name] = data
    try:
        value = json.loads(files["VIRUSTOTAL.json"])
    except (ValueError, UnicodeError):
        refuse("VIRUSTOTAL.json", "invalid-json")
    expected = inventory(ordinary)
    if (not isinstance(value, dict) or value.get("schema") != "amqic-vt-metadata/1"
            or value.get("ordinaryEtag") != tag("publication-ordinary-v1", ordinary_descriptor(ordinary))
            or not isinstance(value.get("files"), list)):
        refuse("VIRUSTOTAL.json", "invalid-vt-binding")
    rows = value["files"]
    statuses = {"not_queried", "not_found", "known", "unsupported", "failed", "unknown"}
    if len(rows) != len(expected):
        refuse("VIRUSTOTAL.json", "vt-membership-mismatch")
    for row, item in zip(rows, expected):
        if (not isinstance(row, dict) or any(row.get(k) != v for k, v in item.items())
                or row.get("status") not in statuses):
            refuse("VIRUSTOTAL.json", "vt-member-or-status-mismatch")
    return files


def snapshot(source, metadata_dir=None):
    files, raw = collect(source)
    if not files:
        refuse("<source>", "empty-source")
    descriptor = ordinary_descriptor(files)
    ordinary_etag = tag("publication-ordinary-v1", descriptor)
    metadata = vt_metadata(metadata_dir, files)
    inputs = dict(schema=SCHEMA, scope="source-and-approved-metadata", source=raw,
                  ordinary=descriptor, metadata=inventory(metadata))
    input_etag = tag("publication-input-v1", inputs)
    manifest = result("inventory", ordinary_etag, ordinaryEtag=ordinary_etag,
                      **{k: v for k, v in descriptor.items() if k != "schema"},
                      generatedMetadata=dict(classification="generated-not-recursively-self-scanned",
                                             files=sorted(metadata), checksumExcludes=["SHA256SUMS"]))
    payload = dict(files, **metadata)
    payload["FILES.json"] = canonical(manifest)
    payload["SHA256SUMS"] = checksums(payload)
    limits(payload)
    return payload, result("plan", input_etag, ordinaryEtag=ordinary_etag,
                           files=inventory(payload), ordinaryFiles=inventory(files))


def checksums(files):
    return "".join(digest(data) + "  " + name + "\n" for name, data in sorted(files.items())
                   if name != "SHA256SUMS").encode("utf-8")


def plan(source, metadata_dir=None):
    return snapshot(source, metadata_dir)[1]


def write_new(path, data):
    with path.open("xb") as stream:
        stream.write(data)


def stage(source, destination, expected_etag, metadata_dir=None):
    payload, planned = snapshot(source, metadata_dir)
    check_expected(expected_etag, planned["etag"])
    dest = checked_absolute(destination, new=True)
    if dest.exists():
        refuse("<destination>", "already-exists")
    src = Path(os.path.abspath(source))
    if dest == src or src in dest.parents:
        refuse("<destination>", "inside-source")
    # Revalidate immediately before the first effect; captured bytes are copied,
    # never reopened after admission. A later source change refuses completion
    # and leaves the exclusive partial destination for explicit reconciliation.
    if snapshot(source, metadata_dir)[1] != planned:
        refuse("<source>", "changed-before-stage")
    dest.mkdir()
    for name, data in sorted(payload.items()):
        path = dest / name
        path.parent.mkdir(parents=True, exist_ok=True)
        write_new(path, data)
    if snapshot(source, metadata_dir)[1] != planned:
        refuse("<source>", "changed-during-stage-partial-retained")
    return verify(dest)


def verify(payload):
    files, _ = collect(payload, payload=True)
    if not {"FILES.json", "SHA256SUMS"}.issubset(files):
        refuse("<payload>", "missing-integrity-files")
    try:
        manifest = json.loads(files["FILES.json"])
    except ValueError:
        refuse("FILES.json", "invalid-json")
    ordinary = {k: v for k, v in files.items() if k not in GENERATED}
    descriptor = ordinary_descriptor(ordinary)
    if not isinstance(manifest, dict):
        refuse("FILES.json", "invalid-manifest")
    unsigned = dict(manifest)
    etag = unsigned.pop("etag", None)
    if (etag != tag("publication-result-v1", unsigned)
            or manifest.get("schema") != SCHEMA or manifest.get("operation") != "inventory"
            or not STRONG.fullmatch(str(manifest.get("inputEtag", "")))
            or any(manifest.get(k) != v for k, v in descriptor.items())
            or manifest.get("ordinaryEtag") != tag("publication-ordinary-v1", descriptor)):
        refuse("FILES.json", "manifest-mismatch")
    vt_names = set(files) & VT_FILES
    if vt_names and vt_names != VT_FILES:
        refuse("<payload>", "exact-vt-pair-required")
    generated = dict(classification="generated-not-recursively-self-scanned",
                     files=sorted(vt_names), checksumExcludes=["SHA256SUMS"])
    if manifest.get("generatedMetadata") != generated:
        refuse("FILES.json", "generated-metadata-mismatch")
    if manifest["inputEtag"] != manifest["ordinaryEtag"]:
        refuse("FILES.json", "ordinary-input-binding-mismatch")
    if vt_names:
        validate_embedded_vt(files, ordinary)
    if files["SHA256SUMS"] != checksums(files):
        refuse("SHA256SUMS", "checksum-mismatch")
    actual = inventory(files)
    return result("payload", manifest["inputEtag"], ordinaryEtag=manifest["ordinaryEtag"], files=actual)


def validate_embedded_vt(files, ordinary):
    # Use the same validation without creating files or recursively scanning the
    # metadata producer's own output. Only ordinary files are submitted/queried.
    try:
        value = json.loads(files["VIRUSTOTAL.json"])
    except ValueError:
        refuse("VIRUSTOTAL.json", "invalid-json")
    expected = inventory(ordinary)
    if (not isinstance(value, dict) or value.get("schema") != "amqic-vt-metadata/1"
            or value.get("ordinaryEtag") != tag("publication-ordinary-v1", ordinary_descriptor(ordinary))
            or not isinstance(value.get("files"), list) or len(value["files"]) != len(expected)):
        refuse("VIRUSTOTAL.json", "invalid-vt-binding")
    for row, item in zip(value["files"], expected):
        if (not isinstance(row, dict) or any(row.get(k) != v for k, v in item.items())
                or row.get("status") not in {"not_queried", "not_found", "known", "unsupported", "failed", "unknown"}):
            refuse("VIRUSTOTAL.json", "vt-member-or-status-mismatch")


def verify_archive(path, expected_files):
    # Archive may exceed an individual source file; bound it independently.
    check_node(path)
    if path.stat().st_size > MAX_TOTAL_BYTES + MAX_FILES * 1024:
        refuse("<archive>", "archive-byte-limit")
    with zipfile.ZipFile(path, "r") as archive:
        entries = archive.infolist()
        if len(entries) != len(expected_files):
            refuse("<archive>", "archive-membership-mismatch")
        names = set()
        actual = {}
        for info in entries:
            safe_name(info.filename)
            mode = info.external_attr >> 16
            if (info.filename.casefold() in names or info.is_dir()
                    or stat.S_IFMT(mode) not in (0, stat.S_IFREG)
                    or info.file_size > MAX_FILE_BYTES or info.flag_bits & 1):
                refuse(info.filename, "invalid-archive-member")
            names.add(info.filename.casefold())
            if info.filename not in expected_files:
                refuse(info.filename, "unexpected-archive-member")
            with archive.open(info, "r") as stream:
                body = stream.read(MAX_FILE_BYTES + 1)
            if body != expected_files[info.filename]:
                refuse(info.filename, "archive-content-mismatch")
            actual[info.filename] = body
        if inventory(actual) != inventory(expected_files):
            refuse("<archive>", "archive-inventory-mismatch")
    return inventory(actual)


def archive_payload(payload, output, expected_etag):
    verified = verify(payload)
    check_expected(expected_etag, verified["etag"])
    files, _ = collect(payload, payload=True)
    if inventory(files) != verified["files"]:
        refuse("<payload>", "changed-before-archive")
    dest = checked_absolute(output, new=True)
    root = Path(os.path.abspath(payload))
    if dest.exists() or dest == root or root in dest.parents:
        refuse("<archive>", "existing-or-inside-payload")
    if verify(payload) != verified:
        refuse("<payload>", "changed-before-archive")
    with zipfile.ZipFile(dest, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for name, data in sorted(files.items()):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data, compresslevel=9)
    members = verify_archive(dest, files)
    if verify(payload) != verified:
        refuse("<payload>", "changed-during-archive-output-retained")
    with dest.open("rb") as stream:
        archive_hash = hashlib.file_digest(stream, "sha256").hexdigest()
    return result("archive", verified["etag"], files=members, archiveSha256=archive_hash,
                  archiveBytes=dest.stat().st_size, readback="exact-members-and-bytes")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("plan", "stage"):
        p = sub.add_parser(command)
        p.add_argument("--source", required=True)
        p.add_argument("--metadata-dir")
        if command == "stage":
            p.add_argument("--destination", required=True)
            p.add_argument("--expected-etag", required=True)
    p = sub.add_parser("verify")
    p.add_argument("--payload", required=True)
    p = sub.add_parser("archive")
    p.add_argument("--payload", required=True)
    p.add_argument("--output", required=True)
    p.add_argument("--expected-etag", required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "plan":
            value = plan(args.source, args.metadata_dir)
        elif args.command == "stage":
            value = stage(args.source, args.destination, args.expected_etag, args.metadata_dir)
        elif args.command == "verify":
            value = verify(args.payload)
        elif args.command == "archive":
            value = archive_payload(args.payload, args.output, args.expected_etag)
        else:
            raise AssertionError("closed-command-dispatch")
    except Refusal as error:
        print(json.dumps({"schema": SCHEMA, "status": "refused", "finding": str(error)}), file=sys.stderr)
        return 2
    except (OSError, ValueError, zipfile.BadZipFile):
        # OS errors may contain private absolute paths. Keep only the rule.
        print(json.dumps({"schema": SCHEMA, "status": "refused", "finding": "<operation>:io-or-format-error-partial-output-may-remain"}), file=sys.stderr)
        return 2
    sys.stdout.buffer.write(canonical(value))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
