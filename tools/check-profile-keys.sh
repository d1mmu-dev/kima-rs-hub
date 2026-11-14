#!/usr/bin/env bash
# M-PROF grep gate (D-240): personal data is keyed by `profile_id` since migration 0060.
# Fails when `user_id` appears inside a string literal (SQL text, runtime or `query!`) in
# crates/*/src/**/*.rs, outside the allow-list:
#   - crates/kima-db/src/users.rs, crates/kima-db/src/profiles.rs (account and profile tables);
#   - `pr.user_id` / `profiles.user_id`: the account column of `profiles`, which is how a
#     query reaches the account of a profile (alias `profiles` as `pr` for that).
# It also fails on a join that treats a profile id as an account id (`u.id = x.profile_id`
# where `u` is an alias of `users`): only a main profile shares its account's id.
# `created_by` (organizer/autotag runs) and `ai_override_by` stay account columns under their
# own names, so they never match. Tests and migrations are not scanned.
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - "$@" <<'PY'
import pathlib, re, sys

ALLOWED_FILES = {
    "crates/kima-db/src/users.rs",
    "crates/kima-db/src/profiles.rs",
}
WORD = re.compile(r"\buser_id\b")
ALLOWED_REF = re.compile(r"\b(pr|profiles)\.user_id\b")
USERS_ALIAS = re.compile(r"\busers\s+(?:AS\s+)?(\w+)", re.I)


def profile_as_account(text):
    """Rows of a literal that compare an alias of `users` with a `profile_id`."""
    for alias in set(USERS_ALIAS.findall(text)):
        a = re.escape(alias)
        pat = re.compile(
            rf"\b{a}\.id\s*=\s*(\w+\.)?profile_id\b|\b(\w+\.)?profile_id\s*=\s*{a}\.id\b"
        )
        for k, row in enumerate(text.split("\n")):
            if pat.search(row):
                yield k


def literals(src):
    """Yields (line, text) for every string literal in Rust source, skipping comments and
    char literals."""
    i, n, line = 0, len(src), 1
    while i < n:
        c = src[i]
        if c == "\n":
            line += 1
            i += 1
        elif src.startswith("//", i):
            j = src.find("\n", i)
            i = n if j < 0 else j
        elif src.startswith("/*", i):
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    line += src[i] == "\n"
                    i += 1
        elif c == "'":
            # A char literal ('x', '\n', '"'), otherwise a lifetime or label.
            if src.startswith("\\", i + 1):
                j = src.find("'", i + 2)
                i = j + 1
            elif i + 2 < n and src[i + 2] == "'":
                i += 3
            else:
                i += 1
        elif (m := re.compile(r'b?r(#*)"').match(src, i)) and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            close = '"' + m.group(1)
            start = m.end()
            end = src.find(close, start)
            text = src[start:end]
            yield line, text
            line += text.count("\n")
            i = end + len(close)
        elif c == '"' or (c == "b" and src.startswith('b"', i)):
            j = i + (2 if c == "b" else 1)
            start, start_line = j, line
            while j < n and src[j] != '"':
                if src[j] == "\\":
                    j += 1
                j += 1
            text = src[start:j]
            yield start_line, text
            line += text.count("\n")
            i = j + 1
        else:
            i += 1


bad = []
for path in sorted(pathlib.Path("crates").glob("*/src/**/*.rs")):
    rel = path.as_posix()
    if rel in ALLOWED_FILES:
        continue
    for line, text in literals(path.read_text()):
        rows = text.split("\n")
        hits = {k for k, row in enumerate(rows) if WORD.search(ALLOWED_REF.sub("", row))}
        hits.update(profile_as_account(text))
        for k in sorted(hits):
            bad.append(f"{rel}:{line + k}: {rows[k].strip()}")
if bad:
    print("user_id in SQL text outside the allow-list (re-key to profile_id, see tools/check-profile-keys.sh):")
    print("\n".join(bad))
    sys.exit(1)
print("profile keys ok")
PY
