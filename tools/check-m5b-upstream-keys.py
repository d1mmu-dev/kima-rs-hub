#!/usr/bin/env python3
"""Check m5b_upstream_keys.json against the upstream sources at the M5b reference (Task 19).

Independent of the hand-written key file's own claims: for every fixture entry, each upstream key name (the last
`.name` segment of a path) must occur as a word in the upstream source lines the entry cites (`path:first-last` at
the reference commit), and each `kimaOnly` key name must NOT occur there (so a key upstream really sends cannot
hide in the kima-only list). Every kimaOnly entry must name a divergence row that exists in docs/DIVERGENCES.md.
`sameShapeAs` entries must point at a real entry. The mix track rows must carry exactly the Prisma `Track` and
`Album` scalar columns of the reference's schema (owner Q6 = B), read here from the schema itself.

Usage: tools/check-m5b-upstream-keys.py <kima-hub checkout> [commit]   (default 9450c94b, the M5b reference)
Exit 0 when everything matches; prints each mismatch otherwise.
"""
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
KEYS = ROOT / "crates/kima-api-types/tests/fixtures/m5b_upstream_keys.json"
DIVERGENCES = ROOT / "docs/DIVERGENCES.md"
# Entries whose `tracks[]` are full Prisma rows (`loadTracksInMixOrder`), and where they sit.
TRACK_ROWS = {"mix": ".tracks[]", "heads_bucket": ".tracks[]", "bucket_save": ".mix.tracks[]"}


def leaf(path):
    """`.tracks[].album.title` -> `title`; element/record/root paths have no key name."""
    if path == "" or path.endswith(("[]", "{}")):
        return None
    return re.split(r"[.\[\]{}]+", path)[-1] or None


def show(repo, commit, file, cache):
    if file not in cache:
        cache[file] = subprocess.run(
            ["git", "-C", repo, "show", f"{commit}:{file}"], check=True, capture_output=True, text=True,
        ).stdout.splitlines()
    return cache[file]


def source_text(repo, commit, spec, cache):
    file, _, span = spec.rpartition(":")
    first, _, last = span.partition("-")
    lines = show(repo, commit, file, cache)
    first, last = int(first), int(last)
    if not (1 <= first <= last <= len(lines)):
        raise SystemExit(f"{spec}: line range outside {file} ({len(lines)} lines)")
    return "\n".join(lines[first - 1:last])


def prisma_scalars(repo, commit, name, cache):
    """The scalar fields Prisma returns for `model <name>` (relations and Unsupported fields left out)."""
    lines = show(repo, commit, "backend/prisma/schema.prisma", cache)
    start = next(i for i, l in enumerate(lines) if l.startswith(f"model {name} "))
    models = {l.split()[1] for l in lines if l.startswith(("model ", "enum "))}
    enums = {l.split()[1] for l in lines if l.startswith("enum ")}
    out = []
    for line in lines[start + 1:]:
        if line.startswith("}"):
            break
        m = re.match(r"\s+(\w+)\s+(\w+)", line)
        if not m or line.strip().startswith(("//", "@@")):
            continue
        key, ty = m.groups()
        if ty == "Unsupported" or (ty in models and ty not in enums):
            continue
        out.append(key)
    return out


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    repo = sys.argv[1]
    commit = sys.argv[2] if len(sys.argv) == 3 else "9450c94b"
    doc = json.loads(KEYS.read_text())
    rows = set(re.findall(r"^\| (D-\d+) \|", DIVERGENCES.read_text(), re.M))
    cache, problems, checked = {}, [], 0
    for name, entry in doc.items():
        if name.startswith("_"):
            continue
        if "sameShapeAs" in entry:
            target = doc.get(entry["sameShapeAs"])
            if not isinstance(target, dict) or "upstream" not in target:
                problems.append(f"{name}: sameShapeAs {entry['sameShapeAs']!r} is not an entry")
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
    track = prisma_scalars(repo, commit, "Track", cache)
    album = prisma_scalars(repo, commit, "Album", cache)
    for name, rows_at in TRACK_ROWS.items():
        upstream = doc[name]["upstream"]
        direct = lambda prefix: {p[len(prefix) + 1:] for p in upstream
                                 if p.startswith(prefix + ".") and "." not in p[len(prefix) + 1:]
                                 and "[" not in p[len(prefix) + 1:]}
        got_track = direct(rows_at) - {"album"}
        got_album = direct(rows_at + ".album") - {"artist"}
        if got_track != set(track):
            problems.append(f"{name}: track columns differ from Prisma Track: "
                            f"missing {sorted(set(track) - got_track)}, extra {sorted(got_track - set(track))}")
        if got_album != set(album):
            problems.append(f"{name}: album columns differ from Prisma Album: "
                            f"missing {sorted(set(album) - got_album)}, extra {sorted(got_album - set(album))}")
        if direct(rows_at + ".album.artist") != {"id", "name", "mbid"}:
            problems.append(f"{name}: album.artist is not upstream's select {{id, name, mbid}}")
    for problem in problems:
        print(problem)
    print(f"{checked} upstream key names checked against {commit}; Track {len(track)} and Album {len(album)} "
          f"columns; {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
