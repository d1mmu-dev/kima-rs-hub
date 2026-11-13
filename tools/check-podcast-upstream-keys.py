#!/usr/bin/env python3
"""Check podcasts_upstream_keys.json against the pinned upstream sources (M6 Task 19).

Independent of the hand-written key file's own claims: for every fixture entry, each upstream
key name (the last `.name` segment of a path) must occur as a word in the upstream source lines
the entry cites (`path:first-last` at the pinned commit), and each `kimaOnly` key name must NOT
occur there (so a key upstream really sends cannot hide in the kima-only list). Every kimaOnly
entry must name a divergence row that exists in docs/DIVERGENCES.md.

Usage: tools/check-podcast-upstream-keys.py <kima-hub checkout> [commit]   (default c7527577)
Exit 0 when everything matches; prints each mismatch otherwise.
"""
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
KEYS = ROOT / "crates/kima-api-types/tests/fixtures/podcasts_upstream_keys.json"
DIVERGENCES = ROOT / "docs/DIVERGENCES.md"


def leaf(path):
    """`[].episodes[].title` -> `title`; element/record/root paths have no key name."""
    if path == "" or path.endswith(("[]", "{}")):
        return None
    return re.split(r"[.\[\]{}]+", path)[-1] or None


def source_text(repo, commit, spec, cache):
    file, _, span = spec.rpartition(":")
    first, _, last = span.partition("-")
    if file not in cache:
        cache[file] = subprocess.run(
            ["git", "-C", repo, "show", f"{commit}:{file}"],
            check=True, capture_output=True, text=True,
        ).stdout.splitlines()
    lines = cache[file]
    first, last = int(first), int(last)
    if not (1 <= first <= last <= len(lines)):
        raise SystemExit(f"{spec}: line range outside {file} ({len(lines)} lines)")
    return "\n".join(lines[first - 1:last])


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    repo = sys.argv[1]
    commit = sys.argv[2] if len(sys.argv) == 3 else "c7527577"
    doc = json.loads(KEYS.read_text())
    rows = set(re.findall(r"^\| (D-\d+) \|", DIVERGENCES.read_text(), re.M))
    cache, problems, checked = {}, [], 0
    for name, entry in doc.items():
        if name.startswith("_") or "sameShapeAs" in entry:
            continue
        text = "\n".join(source_text(repo, commit, s, cache) for s in entry["sources"])
        for path in entry["upstream"]:
            key = leaf(path)
            if key is None:
                continue
            checked += 1
            if not re.search(rf"\b{re.escape(key)}\b", text):
                problems.append(f"{name}: upstream key `{path}` not found in {entry['sources']}")
        for path, spec in entry["kimaOnly"].items():
            key = leaf(path)
            if spec.get("divergence") not in rows:
                problems.append(f"{name}: kimaOnly `{path}` cites no DIVERGENCES row ({spec.get('divergence')})")
            if key and entry["sources"] and re.search(rf"\b{re.escape(key)}\b", text):
                problems.append(f"{name}: kimaOnly `{path}` is in upstream's cited source")
    for problem in problems:
        print(problem)
    print(f"{checked} upstream key names checked against {commit}; {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
