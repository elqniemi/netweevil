#!/usr/bin/env python3
"""Extract the pinned official OSRM Linux amd64 image without a container daemon."""

import argparse
import hashlib
import json
import shutil
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath


REPOSITORY = "project-osrm/osrm-backend"
DIGEST = "sha256:c29a50d67b9be17d10773fa2b52bb045ee3fbb8f42e5f9d1c671ce0d9bb21f37"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, default=Path("/tmp/netweevil-osrm-image"))
    args = parser.parse_args()
    root = args.out.resolve()
    root.mkdir(parents=True, exist_ok=True)
    auth_url = "https://ghcr.io/token?service=ghcr.io&scope=repository:" + REPOSITORY + ":pull"
    with urllib.request.urlopen(auth_url) as response:
        token = json.load(response)["token"]
    headers = {
        "Authorization": "Bearer " + token,
        "Accept": "application/vnd.oci.image.manifest.v1+json",
    }
    base = "https://ghcr.io/v2/" + REPOSITORY

    def fetch(path):
        return urllib.request.urlopen(urllib.request.Request(base + path, headers=headers))

    with fetch("/manifests/" + DIGEST) as response:
        manifest_bytes = response.read()
    if hashlib.sha256(manifest_bytes).hexdigest() != DIGEST.split(":")[1]:
        raise ValueError("Image manifest checksum mismatch")
    manifest = json.loads(manifest_bytes)
    for layer in manifest["layers"]:
        archive = root / (layer["digest"].split(":")[1] + ".tar.gz")
        if not archive.exists():
            partial = archive.with_suffix(".partial")
            with fetch("/blobs/" + layer["digest"]) as source, partial.open("wb") as output:
                shutil.copyfileobj(source, output)
            partial.replace(archive)
        with archive.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if digest != layer["digest"].split(":")[1]:
            raise ValueError("Image layer checksum mismatch: " + str(archive))
        with tarfile.open(archive) as tar:
            for member in tar:
                path = PurePosixPath(member.name)
                if path.is_absolute() or ".." in path.parts or not member.isfile():
                    continue
                if not str(path).startswith(("usr/local/bin/", "opt/")):
                    continue
                target = root / str(path)
                if not target.resolve().is_relative_to(root):
                    raise ValueError("Output path escapes destination: " + str(target))
                target.parent.mkdir(parents=True, exist_ok=True)
                with tar.extractfile(member) as source, tempfile.NamedTemporaryFile(
                    dir=target.parent, delete=False
                ) as output:
                    shutil.copyfileobj(source, output)
                    temporary = Path(output.name)
                temporary.chmod(member.mode & 0o777)
                temporary.replace(target)
    print(root / "usr/local/bin/osrm-routed")


if __name__ == "__main__":
    main()
