#!/usr/bin/env bash
# M-PROF grep gate (D-240): personal data is keyed by `profile_id` since migration 0060.
# Fails when `user_id` appears inside a string literal (SQL text, runtime or `query!`) in
# crates/*/src/**/*.rs, outside the allow-list:
#   - crates/kima-db/src/users.rs, crates/kima-db/src/profiles.rs (account and profile tables);
#   - `pr.user_id` / `profiles.user_id`: the account column of `profiles`, which is how a
#     query reaches the account of a profile (alias `profiles` as `pr` for that).
# It also fails on SQL that treats a profile id as an account id, because only a main profile
# shares its account's id: a comparison of `users`' id (aliased or not, also split over lines)
# with a `profile_id` (`JOIN users u ON u.id = x.profile_id`, `JOIN users ON users.id =
# profile_id`, `FROM users u WHERE ... u.id = profile_id`), and a subquery on `users` feeding a
# `profile_id` (`profile_id IN (SELECT id FROM users ...)`, `profile_id = (SELECT id FROM users`).
# `created_by` (organizer/autotag runs) and `ai_override_by` stay account columns under their
# own names, so they never match. Tests and migrations are not scanned.
#
# Second check: the ways around the `AccountId`/`ProfileId` types (`ProfileId::new(..)`,
# `ProfileId::main_of(..)`, `.main_profile()`) are allowed only where a profile id is ground
# truth or the main profile is meant by definition (ID_FILES), when decoding a database row in
# kima-db (`ProfileId::new(r.<col>)`/`row.`), or on a line marked with an `M-PROF Task N:`
# comment within the four lines above it (a call site a later task moves to the session's
# profile). Code after `#[cfg(test)]` is not scanned.
#
# Third check: a marker must name a task still to come. `DONE_TASKS` lists the plan's finished
# tasks; a `// M-PROF Task N:` marker of one of them in crates/*/src fails the gate, so a task
# cannot be closed with call sites still waiting for it.
#
# `--root DIR` scans DIR instead of the repository (used to try the gate on samples).
set -euo pipefail
cd "$(dirname "$0")/.."
python3 - "$@" <<'PY'
import os, pathlib, re, sys

if sys.argv[1:2] == ["--root"]:
    os.chdir(sys.argv[2])

ALLOWED_FILES = {
    "crates/kima-db/src/users.rs",
    "crates/kima-db/src/profiles.rs",
}
WORD = re.compile(r"\buser_id\b")
ALLOWED_REF = re.compile(r"\b(pr|profiles)\.user_id\b")
USERS_ALIAS = re.compile(r"\busers\s+(?:AS\s+)?(\w+)", re.I)
# Words that can follow `users` without being its alias.
NOT_ALIAS = {
    "on", "where", "join", "left", "right", "inner", "full", "cross", "natural", "using",
    "set", "order", "group", "limit", "for", "returning", "union", "values", "having",
}
USERS_SUBQUERY = re.compile(
    r"\bprofile_id\s*(?:=|IN|=\s*ANY)\s*\(\s*SELECT\s+(?:\w+\.)?id\s+FROM\s+users\b", re.I
)


def profile_as_account(text):
    """Rows of a literal that compare `users`' id (under any alias, or none) with a
    `profile_id`, or feed a `profile_id` from a subquery on `users`. Matches may span lines."""
    if not re.search(r"\busers\b", text, re.I):
        return
    # Escaped whitespace (`\n`, `\t`, a `\` line continuation) is whitespace to SQL; same
    # length, so offsets still map to rows.
    text = re.sub(r"\\[nrt]", "  ", text).replace("\\\n", " \n")
    aliases = {"users"}
    aliases.update(a for a in USERS_ALIAS.findall(text) if a.lower() not in NOT_ALIAS)
    pats = [USERS_SUBQUERY]
    for alias in aliases:
        a = re.escape(alias)
        pats.append(
            re.compile(
                rf"\b{a}\.id\s*=\s*(\w+\.)?profile_id\b|\b(\w+\.)?profile_id\s*=\s*{a}\.id\b",
                re.I,
            )
        )
        pats.append(re.compile(rf"\b{a}\.id\s+IN\s*\(\s*SELECT\s+(\w+\.)?profile_id\b", re.I))
    # `users` unqualified: `JOIN users ON id = profile_id`, `FROM users WHERE id = p.profile_id`.
    pats.append(
        re.compile(r"\busers\s+(?:ON|WHERE)\s+id\s*(?:=|IN\s*\(\s*SELECT)\s*(\w+\.)?profile_id\b", re.I)
    )
    for pat in pats:
        for m in pat.finditer(text):
            yield text.count("\n", 0, m.start())


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


# Where a bare profile id is ground truth (database rows, token claims, the newtypes) or the
# main profile is meant by definition (sign-in and registration land in it; a Subsonic password
# sign-in lands in it, answer Q7).
ID_FILES = (
    "crates/kima-core/src/profile.rs",
    "crates/kima-core/src/auth.rs",
    "crates/kima-db/src/users.rs",
    "crates/kima-db/src/profiles.rs",
    "crates/kima-api/src/auth.rs",
    "crates/kima-api/src/routes/auth.rs",
    "crates/kima-api/src/routes/onboarding.rs",
    # Test seeds: an account's main profile is what they set up.
    "crates/kima-test-support/src/profiles.rs",
)
# Files where only the named functions may name the main profile: a Subsonic password sign-in
# lands in the main profile (answer Q7); anything else in that file goes through the gate,
# including items after the function's closing brace (methods, `impl` blocks, nested fns).
ID_FUNCS = {
    "crates/kima-subsonic/src/auth.rs": {"main_session"},
}
# A function header at any indentation (free functions, methods, nested functions).
FN_START = re.compile(r"^\s*(?:pub(?:\([^)]*\))? )?(?:const )?(?:async )?(?:unsafe )?fn (\w+)")
ID_ESCAPE = re.compile(r"ProfileId::new\(|\bmain_of\(|\.main_profile\(\)")
# A row of a kima-db query decoded into the newtype: the column is ground truth.
ROW_DECODE = re.compile(r"ProfileId::new\((?:r|row)\.\w+\??\)")
MARKER = re.compile(r"//.*M-PROF Task \d+:")
# Plan tasks already done (docs/superpowers/plans/2026-11-14-m-prof.md); add each on closing it.
DONE_TASKS = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9}
TASK_MARKER = re.compile(r"//.*M-PROF Task (\d+):")


def id_escapes(rel, src):
    if rel.startswith(ID_FILES):
        return
    lines = src.split("\n")
    allowed_fns = ID_FUNCS.get(rel, set())
    # Brace depth inside an allowed function; None outside. The allowance ends where that
    # function's body closes (braces counted outside comments), not at the next `fn`.
    depth, opened = None, False
    for k, row in enumerate(lines):
        if row.strip().startswith("#[cfg(test)]"):
            return
        code = row.split("//", 1)[0]
        if depth is None and (m := FN_START.match(row)) and m.group(1) in allowed_fns:
            depth, opened = 0, False
        if depth is not None:
            depth += code.count("{") - code.count("}")
            opened = opened or "{" in code
            if (opened and depth <= 0) or (not opened and ";" in code):
                depth = None
            continue
        if rel.startswith("crates/kima-db/src/"):
            code = ROW_DECODE.sub("", code)
        if ID_ESCAPE.search(code) and not any(MARKER.search(r) for r in lines[max(0, k - 4) : k + 1]):
            yield k + 1, row.strip()


bad = []
escapes = []
stale = []
for path in sorted(pathlib.Path("crates").glob("*/src/**/*.rs")):
    rel = path.as_posix()
    src = path.read_text()
    escapes.extend(f"{rel}:{k}: {row}" for k, row in id_escapes(rel, src))
    stale.extend(
        f"{rel}:{k}: {row.strip()}"
        for k, row in enumerate(src.split("\n"), 1)
        if (m := TASK_MARKER.search(row)) and int(m.group(1)) in DONE_TASKS
    )
    if rel in ALLOWED_FILES:
        continue
    for line, text in literals(src):
        rows = text.split("\n")
        hits = {k for k, row in enumerate(rows) if WORD.search(ALLOWED_REF.sub("", row))}
        hits.update(profile_as_account(text))
        hits = {k for k in hits if k < len(rows)}
        for k in sorted(hits):
            bad.append(f"{rel}:{line + k}: {rows[k].strip()}")
if bad:
    print("user_id in SQL text, or a profile id compared with an account id, outside the allow-list (see tools/check-profile-keys.sh):")
    print("\n".join(bad))
if escapes:
    print("profile id built by hand outside the allow-list (use the session's profile, see tools/check-profile-keys.sh):")
    print("\n".join(escapes))
if stale:
    print("M-PROF marker of a finished task (move the call site, then drop the marker; see tools/check-profile-keys.sh):")
    print("\n".join(stale))
if bad or escapes or stale:
    sys.exit(1)
print("profile keys ok")
PY
