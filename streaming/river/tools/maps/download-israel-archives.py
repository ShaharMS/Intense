#!/usr/bin/env python3
"""Download the Survey of Israel 2015 2 m ZIP sheets from Data.gov.il."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import subprocess
from time import sleep
from urllib.parse import urlencode, urlsplit
from urllib.request import urlopen
from zipfile import is_zipfile

CATALOG = "https://data.gov.il/api/3/action/package_search"
USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0"


def download(directory, url):
    resource = urlsplit(url)
    if resource.scheme != "https" or resource.hostname != "data.gov.il":
        raise ValueError(f"Unexpected archive origin: {url}")
    name = Path(resource.path).name
    output = directory / name
    if output.exists() and is_zipfile(output):
        return
    partial = output.with_suffix(".zip.part")
    for attempt in range(12):
        if is_zipfile(partial):
            partial.replace(output)
            print(f"Downloaded {name}", flush=True)
            return
        result = subprocess.run([
            "curl", "--fail", "--location", "--silent", "--show-error",
            "--connect-timeout", "20", "--speed-limit", "1024", "--speed-time", "60",
            "--user-agent", USER_AGENT, "--continue-at", "-", "--output", str(partial),
            url + "?download=1",
        ], check=False)
        if result.returncode == 0 and is_zipfile(partial):
            partial.replace(output)
            print(f"Downloaded {name}", flush=True)
            return
        sleep(min(2 ** attempt, 30))
    raise RuntimeError(f"Could not download a complete ZIP for {name}")


def download_with_aria2(directory, urls, executable, jobs):
    pending = []
    for url in urls:
        resource = urlsplit(url)
        if resource.scheme != "https" or resource.hostname != "data.gov.il":
            raise ValueError(f"Unexpected archive origin: {url}")
        output = directory / Path(resource.path).name
        if not is_zipfile(output):
            pending.append((url, output, output.with_suffix(".zip.part")))

    if not pending:
        return

    queue = "".join(f"{url}?download=1\n  out={partial.name}\n"
                    for url, _, partial in pending)
    result = subprocess.run([
        executable, "--no-conf=true", "--input-file=-", f"--dir={directory.resolve()}",
        f"--max-concurrent-downloads={jobs}", "--continue=true", "--file-allocation=none",
        "--split=1", "--disable-ipv6=true", "--auto-file-renaming=false",
        "--connect-timeout=20", "--timeout=60", "--lowest-speed-limit=1024",
        "--max-tries=5", "--retry-wait=15", "--summary-interval=30",
        "--show-console-readout=false", "--enable-color=false", f"--user-agent={USER_AGENT}",
    ], input=queue, text=True, check=False)
    missing = []
    for _, output, partial in pending:
        if not Path(str(partial) + ".aria2").exists() and is_zipfile(partial):
            partial.replace(output)
            print(f"Downloaded {output.name}", flush=True)
        else:
            missing.append(output.name)
    if missing:
        raise RuntimeError(f"aria2c exited with status {result.returncode}; "
                           f"{len(missing)} archives still need downloading")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--aria2c", metavar="EXECUTABLE", help="Resume the queue with aria2c")
    parser.add_argument("--corpus", type=Path, default=Path("corpus"))
    args = parser.parse_args()
    directory = args.corpus / "maps" / "sources" / "israel-archives"
    directory.mkdir(parents=True, exist_ok=True)
    query = urlencode({"fq": "organization:israel_mapping_center", "rows": 1000})
    with urlopen(CATALOG + "?" + query, timeout=30) as response:
        packages = json.load(response)["result"]["results"]
    urls = [
        resource["url"] for package in packages if "2015 2 מטר" in package["title"]
        for resource in package["resources"] if resource["format"].upper() == "ZIP"
    ]
    if not urls:
        raise RuntimeError("No 2015 Survey of Israel 2 m archive links found")
    print(f"Downloading {len(urls)} source sheets to {directory}", flush=True)
    if args.aria2c:
        download_with_aria2(directory, urls, args.aria2c, args.jobs)
        return
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for _ in pool.map(lambda url: download(directory, url), urls):
            pass


if __name__ == "__main__":
    main()
