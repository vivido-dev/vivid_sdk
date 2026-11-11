#!/usr/bin/env python3
"""Verify the private native bundle against its published crate archives."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile


def archive(cache, source):
    name = f"{source['crate']}-{source['version']}.crate"
    matches = list(cache.rglob(name))
    if not matches:
        raise SystemExit(f"Missing cached archive: {name}")
    path = matches[0]
    if hashlib.sha256(path.read_bytes()).hexdigest() != source["sha256"]:
        raise SystemExit(f"Archive checksum mismatch: {name}")
    return path


def files(directory):
    return {path.relative_to(directory): path for path in directory.rglob("*") if path.is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cargo/registry/cache")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    sources = json.loads((root / "sources.json").read_text())
    with tempfile.TemporaryDirectory(prefix="vivid-wayland-verify-") as temp:
        pristine = Path(temp)
        for source in sources:
            with tarfile.open(archive(args.cache, source)) as package:
                package.extractall(pristine, filter="data")
            (pristine / f"{source['crate']}-{source['version']}").rename(pristine / source["crate"])
        subprocess.run(
            ["patch", "--batch", "-p1", "-i", str(root / "vivid-backport.patch")],
            cwd=pristine,
            check=True,
            stdout=subprocess.DEVNULL,
        )
        for source in sources:
            name = source["crate"]
            expected, actual = files(pristine / name), files(root / name)
            if expected.keys() != actual.keys():
                raise SystemExit(f"Unexpected package files: {name}")
            for relative, path in expected.items():
                if path.read_bytes() != actual[relative].read_bytes():
                    raise SystemExit(f"Package differs from recorded patch: {name}/{relative}")
        for source in json.loads((root / "test-fixtures.json").read_text()):
            with tarfile.open(archive(args.cache, source)) as package:
                data = package.extractfile(source["member"]).read()
            if hashlib.sha256(data).hexdigest() != source["file_sha256"]:
                raise SystemExit("Test fixture checksum mismatch")
            if data != (root / source["destination"]).read_bytes():
                raise SystemExit("Test fixture differs from upstream")
    print("Verified eight packages, recorded patch and upstream test fixture.")


if __name__ == "__main__":
    main()
