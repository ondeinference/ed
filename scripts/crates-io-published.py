#!/usr/bin/env python3
"""Check whether a crate version is visible in the crates.io sparse index.

Exit 0 when published, 1 when absent, and 2 when the registry could not be
queried.  Do not use `cargo info` for this: inside a workspace it can resolve
the local package and report a version as published when it is not.
"""

import argparse
import json
import sys
import time
import urllib.error
import urllib.request


def index_path(crate: str) -> str:
    """Return the sparse-index path for a crate name."""
    name = crate.lower()
    if len(name) == 1:
        return f"1/{name}"
    if len(name) == 2:
        return f"2/{name}"
    if len(name) == 3:
        return f"3/{name[0]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def versions(crate: str) -> list[str]:
    request = urllib.request.Request(
        f"https://index.crates.io/{index_path(crate)}",
        headers={"User-Agent": "ed-release (https://github.com/ondeinference/ed)"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return []
        raise
    return [json.loads(line)["vers"] for line in body.splitlines() if line.strip()]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version")
    parser.add_argument("--crate", required=True)
    parser.add_argument("--wait", action="store_true")
    parser.add_argument("--attempts", type=int, default=10)
    parser.add_argument("--interval", type=int, default=15)
    args = parser.parse_args()

    total_attempts = args.attempts if args.wait else 1
    for attempt in range(1, total_attempts + 1):
        try:
            if args.version in versions(args.crate):
                print(f"{args.crate} {args.version} is on crates.io")
                return 0
        except (OSError, ValueError, urllib.error.URLError) as error:
            print(f"could not reach the crates.io index: {error}", file=sys.stderr)
            return 2

        if attempt < total_attempts:
            print(
                f"attempt {attempt}: {args.crate} {args.version} is not visible; "
                f"waiting {args.interval}s"
            )
            time.sleep(args.interval)

    print(f"{args.crate} {args.version} is not on crates.io")
    return 1


if __name__ == "__main__":
    sys.exit(main())
