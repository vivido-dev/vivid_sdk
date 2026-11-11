#!/usr/bin/env python3
"""Reproduce the private Parley source and verify its pinned test fixtures."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cargo/registry/cache")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    source = json.loads((root / "sources.json").read_text())
    name = f"{source['crate']}-{source['version']}"
    archives = list(args.cache.rglob(name + ".crate"))
    if not archives or digest(archives[0]) != source["archive_sha256"]:
        raise SystemExit("Missing archive or archive checksum mismatch")
    with tempfile.TemporaryDirectory(prefix="vivid-parley-verify-") as temporary:
        pristine = Path(temporary)
        with tarfile.open(archives[0]) as archive:
            for member in archive.getmembers():
                relative = Path(member.name).relative_to(name)
                if relative.is_absolute() or ".." in relative.parts:
                    raise SystemExit("Invalid archive member")
                if member.isfile():
                    target = pristine / relative
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(archive.extractfile(member).read())
                elif not member.isdir():
                    raise SystemExit("Unexpected archive link")
        subprocess.run(
            ["patch", "--batch", "-p1", "-i", str(root / "vivid-backport.patch")],
            cwd=pristine, check=True, stdout=subprocess.DEVNULL,
        )
        expected = {p.relative_to(pristine) for p in pristine.rglob("*") if p.is_file()}
        metadata = {Path(p) for p in ["UPSTREAM.md", "sources.json", "verify_sources.py", "vivid-backport.patch"]}
        fixtures = {Path(p) for p in source["test_fixtures"]}
        actual = {
            p.relative_to(root) for p in root.rglob("*")
            if p.is_file() and not any(part in {"target", "__pycache__"} for part in p.relative_to(root).parts)
        } - metadata - fixtures
        if expected != actual:
            raise SystemExit("Unexpected vendored files")
        for relative in expected:
            if (pristine / relative).read_bytes() != (root / relative).read_bytes():
                raise SystemExit(f"Source differs from recorded patch: {relative}")
        for relative, fixture in source["test_fixtures"].items():
            if digest(root / relative) != fixture["sha256"]:
                raise SystemExit(f"Fixture checksum mismatch: {relative}")
    print(f"Verified Parley archive, {len(expected)} source files and {len(fixtures)} fixtures.")


if __name__ == "__main__":
    main()
