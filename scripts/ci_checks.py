#!/usr/bin/env python3
"""Verify a clean committed export and assemble a source-only CI artifact."""
import argparse
import json

import prepare_repository as publication


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, help="clean git archive export")
    parser.add_argument("--output", required=True, help="new directory outside source")
    args = parser.parse_args()
    source = publication.checked_absolute(args.source)
    output = publication.checked_absolute(args.output, new=True)
    if output.exists() or output == source or source in output.parents:
        raise publication.Refusal("use a new output directory outside source")
    original = publication.verify(source)
    output.mkdir()
    metadata = output / "metadata"
    metadata.mkdir()
    for name in ("VIRUSTOTAL.json", "VIRUSTOTAL.md"):
        publication.write_new(metadata / name, publication.read_bytes(source / name))
    plan = publication.plan(source, metadata)
    staged = publication.stage(source, output / "payload", plan["etag"], metadata)
    if staged["files"] != original["files"] or staged["etag"] != original["etag"]:
        raise publication.Refusal("source reconstruction differs from committed export")
    archive = publication.archive_payload(
        output / "payload", output / "source.zip", staged["etag"]
    )
    publication.write_new(output / "publication.json", publication.canonical(archive))
    print(json.dumps({"files": len(staged["files"]),
                      "etag": staged["etag"],
                      "archiveSha256": archive["archiveSha256"],
                      "readback": archive["readback"]}))


if __name__ == "__main__":
    main()
