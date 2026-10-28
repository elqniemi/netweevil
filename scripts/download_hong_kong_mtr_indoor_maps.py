#!/usr/bin/env python3
"""Download the LandsD 3D Indoor Map archives for every indexed MTR station."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import re
import shutil
import sqlite3
import sys
import time
import urllib.error
import urllib.request
import zipfile
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable


USER_AGENT = "netweevil-hong-kong-indoor-map-downloader/1.0"


@dataclass(frozen=True)
class StationResource:
    venue_id: str
    venue_name_en: str
    venue_name_zh: str
    revision_date: str | None
    source_url: str


@dataclass(frozen=True)
class DownloadResult:
    venue_id: str
    venue_name_en: str
    venue_name_zh: str
    revision_date: str | None
    source_url: str
    archive: str
    sha256: str
    byte_size: int
    status: str
    extracted_to: str | None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Read a LandsD 3D Indoor Map index GeoPackage and download all "
            "MTR station archives referenced by it."
        )
    )
    parser.add_argument("index", type=Path, help="Index GeoPackage path")
    parser.add_argument(
        "--output-dir",
        type=Path,
        required=True,
        help="Directory for downloaded station archives and manifest.json",
    )
    parser.add_argument(
        "--format",
        choices=("geojson", "shapefile"),
        default="geojson",
        help="Indexed archive format to download (default: geojson)",
    )
    parser.add_argument(
        "--jobs",
        type=int,
        default=4,
        help="Concurrent downloads (default: 4)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=120.0,
        help="Per-request timeout in seconds (default: 120)",
    )
    parser.add_argument(
        "--retries",
        type=int,
        default=3,
        help="Attempts per archive (default: 3)",
    )
    parser.add_argument(
        "--extract",
        action="store_true",
        help="Extract each validated archive under output-dir/extracted",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Redownload archives that already exist and pass ZIP validation",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Validate the index and print what would be downloaded",
    )
    return parser.parse_args()


def load_station_resources(index: Path, archive_format: str) -> list[StationResource]:
    if not index.is_file():
        raise FileNotFoundError(f"index GeoPackage does not exist: {index}")

    url_column = "GeoJSON" if archive_format == "geojson" else "Shapefile"
    connection = sqlite3.connect(f"file:{index.resolve()}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            f'''
            SELECT
                TRIM(Venue_ID),
                TRIM(Venue_Eng_name),
                TRIM(Venue_Chi_name),
                REVISIONDATE,
                TRIM("{url_column}")
            FROM "Index"
            WHERE LOWER(TRIM(Type)) = 'trainstation'
            ORDER BY Venue_Eng_name, Venue_ID
            '''
        ).fetchall()
    finally:
        connection.close()

    resources = [
        StationResource(
            venue_id=venue_id,
            venue_name_en=venue_name_en,
            venue_name_zh=venue_name_zh,
            revision_date=revision_date,
            source_url=source_url,
        )
        for venue_id, venue_name_en, venue_name_zh, revision_date, source_url in rows
    ]
    if not resources:
        raise RuntimeError('the "Index" layer contains no trainstation records')

    missing = [resource.venue_name_en for resource in resources if not resource.source_url]
    if missing:
        raise RuntimeError(
            f"{len(missing)} trainstation records have no {url_column} URL: "
            + ", ".join(missing[:5])
        )
    return resources


def slugify(value: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", value.lower()).strip("-")
    return slug or "station"


def archive_path(output_dir: Path, resource: StationResource) -> Path:
    return output_dir / f"{slugify(resource.venue_name_en)}--{resource.venue_id}.zip"


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_zip(path: Path) -> None:
    if not path.is_file() or path.stat().st_size == 0:
        raise RuntimeError(f"empty or missing archive: {path}")
    with zipfile.ZipFile(path) as archive:
        if not archive.namelist():
            raise RuntimeError(f"archive contains no files: {path}")
        corrupt_member = archive.testzip()
        if corrupt_member is not None:
            raise RuntimeError(f"corrupt ZIP member {corrupt_member!r} in {path}")


def safe_extract(path: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    destination_root = destination.resolve()
    with zipfile.ZipFile(path) as archive:
        for member in archive.infolist():
            member_path = (destination / member.filename).resolve()
            if destination_root not in member_path.parents and member_path != destination_root:
                raise RuntimeError(f"unsafe ZIP member path: {member.filename!r}")
        archive.extractall(destination)


def fetch(resource: StationResource, target: Path, timeout: float, retries: int) -> str:
    if retries < 1:
        raise ValueError("retries must be at least 1")

    temporary = target.with_suffix(f".zip.part.{os.getpid()}")
    for attempt in range(1, retries + 1):
        try:
            request = urllib.request.Request(
                resource.source_url,
                headers={"User-Agent": USER_AGENT, "Accept": "application/zip,*/*"},
            )
            with urllib.request.urlopen(request, timeout=timeout) as response:
                with temporary.open("wb") as output:
                    shutil.copyfileobj(response, output, length=1024 * 1024)
            validate_zip(temporary)
            temporary.replace(target)
            return "downloaded"
        except (OSError, RuntimeError, urllib.error.URLError, zipfile.BadZipFile) as error:
            temporary.unlink(missing_ok=True)
            if attempt == retries:
                raise RuntimeError(
                    f"failed to download {resource.venue_name_en} after {retries} attempts: {error}"
                ) from error
            time.sleep(min(2 ** (attempt - 1), 8))
    raise AssertionError("unreachable")


def download_one(
    resource: StationResource,
    output_dir: Path,
    timeout: float,
    retries: int,
    force: bool,
    extract: bool,
) -> DownloadResult:
    target = archive_path(output_dir, resource)
    status = "cached"
    if force or not target.exists():
        status = fetch(resource, target, timeout, retries)
    else:
        try:
            validate_zip(target)
        except (OSError, RuntimeError, zipfile.BadZipFile):
            status = fetch(resource, target, timeout, retries)

    extracted_to: str | None = None
    if extract:
        destination = output_dir / "extracted" / target.stem
        safe_extract(target, destination)
        extracted_to = str(destination)

    return DownloadResult(
        **asdict(resource),
        archive=str(target),
        sha256=sha256_file(target),
        byte_size=target.stat().st_size,
        status=status,
        extracted_to=extracted_to,
    )


def print_dry_run(resources: Iterable[StationResource], archive_format: str) -> None:
    resources = list(resources)
    print(f"Validated {len(resources)} MTR station records ({archive_format}).")
    for resource in resources:
        print(f"- {resource.venue_name_en} [{resource.venue_id}]")


def main() -> int:
    args = parse_args()
    if args.jobs < 1:
        raise ValueError("--jobs must be at least 1")

    resources = load_station_resources(args.index, args.format)
    if args.dry_run:
        print_dry_run(resources, args.format)
        return 0

    args.output_dir.mkdir(parents=True, exist_ok=True)
    results: list[DownloadResult] = []
    failures: list[str] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as executor:
        future_to_resource = {
            executor.submit(
                download_one,
                resource,
                args.output_dir,
                args.timeout,
                args.retries,
                args.force,
                args.extract,
            ): resource
            for resource in resources
        }
        completed = 0
        for future in concurrent.futures.as_completed(future_to_resource):
            resource = future_to_resource[future]
            completed += 1
            try:
                result = future.result()
                results.append(result)
                print(
                    f"[{completed}/{len(resources)}] {result.status}: "
                    f"{resource.venue_name_en} ({result.byte_size} bytes)",
                    flush=True,
                )
            except Exception as error:  # noqa: BLE001 - aggregate all station failures
                failures.append(str(error))
                print(
                    f"[{completed}/{len(resources)}] FAILED: {resource.venue_name_en}: {error}",
                    file=sys.stderr,
                    flush=True,
                )

    results.sort(key=lambda result: (result.venue_name_en, result.venue_id))
    manifest = {
        "index": str(args.index.resolve()),
        "format": args.format,
        "station_count": len(resources),
        "downloaded_count": len(results),
        "failure_count": len(failures),
        "resources": [asdict(result) for result in results],
        "failures": failures,
    }
    manifest_path = args.output_dir / "manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    if failures:
        print(f"Completed with {len(failures)} failure(s); see {manifest_path}", file=sys.stderr)
        return 1
    print(f"Downloaded and verified all {len(results)} station archives.")
    print(f"Manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
