#!/usr/bin/env bash
# Offline tests of the external-reference and kima-web generator tooling. Every git repository here is a FAKE made
# under a temporary directory: the owner's external kima-hub checkout is never touched, nothing is fetched or pulled
# from the network and npm is never run. Needs bash, git, sha256sum and node (Node >= 22) on PATH.
#   tools/tests/reference-tooling.sh
set -uo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/kima-reference-tooling.XXXXXXXX")"
trap 'rm -rf -- "$tmp"' EXIT
fails=0
pass() { echo "PASS $1"; }
fail() { echo "FAIL $1: $2"; fails=$((fails + 1)); }

# Hermetic git: no user/system configuration (signing, insteadOf, hooks) and no inherited repository variables.
export GIT_CONFIG_GLOBAL="$tmp/gitconfig" GIT_CONFIG_NOSYSTEM=1 GIT_TERMINAL_PROMPT=0
unset $(git rev-parse --local-env-vars)
: >"$GIT_CONFIG_GLOBAL"
# The private tools config must not leak into the tests: point it at an empty file.
export KIMA_RS_TOOLS_ENV="$tmp/empty-tools.env"
: >"$KIMA_RS_TOOLS_ENV"
unset KIMA_HUB_REFERENCE KIMA_REFERENCE_COPY KIMA_REFERENCE_FRONTEND KIMA_CSS_DEPS KIMA_TOOLS_PATH VERTIGO FFMPEG UPSTREAM
CANON=https://github.com/Chevron7Locked/kima-hub.git
g() { git -c user.name=test -c user.email=test@example.invalid -c commit.gpgsign=false "$@"; }

# A fake reference checkout: branch main tracking origin/main of a canonical-looking remote, HEAD == upstream.
# The remote-tracking ref is written directly, so nothing is ever fetched.
mkref() {
  local dir="$1" url="${2:-$CANON}"
  mkdir -p "$dir/frontend/lib"
  g -C "$dir" init -q -b main
  echo '{"name":"fake-frontend","private":true}' >"$dir/frontend/package.json"
  printf 'export const a = 1;\n' >"$dir/frontend/lib/a.ts"
  printf 'backend\n' >"$dir/README.md"
  g -C "$dir" add -A && g -C "$dir" commit -q -m init
  g -C "$dir" remote add origin "$url"
  g -C "$dir" update-ref refs/remotes/origin/main HEAD
  g -C "$dir" config branch.main.remote origin
  g -C "$dir" config branch.main.merge refs/heads/main
}
prep() { "$root/tools/prepare-kima-hub-reference.sh" "$@"; }
val() { sed -n "s/^$1=//p" <<<"$2" | head -1; }

# --- Important 2: only the canonical upstream, exactly at its tracked branch tip, is a reference -----------------
mkref "$tmp/ahead"
echo x >>"$tmp/ahead/README.md"; g -C "$tmp/ahead" commit -qam local
if KIMA_HUB_REFERENCE="$tmp/ahead" KIMA_REFERENCE_COPY="$tmp/copy-ahead" prep --no-pull >/dev/null 2>"$tmp/err"; then
  fail ahead-refused "a checkout with a local commit ahead of upstream was accepted"
elif [ -e "$tmp/copy-ahead" ]; then fail ahead-refused "a copy was written before refusing"
else pass ahead-refused; fi

mkref "$tmp/fork" https://github.com/someone-else/kima-hub.git
if KIMA_HUB_REFERENCE="$tmp/fork" KIMA_REFERENCE_COPY="$tmp/copy-fork" prep --no-pull >/dev/null 2>&1; then
  fail fork-refused "a checkout tracking a fork was accepted"
else pass fork-refused; fi

# insteadOf rewrites a canonical-looking URL to somewhere else: the EFFECTIVE fetch URL decides, before any pull.
mkref "$tmp/rewritten"
g -C "$tmp/rewritten" config "url.$tmp/fork.insteadOf" "$CANON"
if KIMA_HUB_REFERENCE="$tmp/rewritten" KIMA_REFERENCE_COPY="$tmp/copy-rw" prep >/dev/null 2>"$tmp/err"; then
  fail insteadof-refused "an insteadOf-redirected remote was accepted (and pulled)"
elif [ -e "$tmp/rewritten/.git/FETCH_HEAD" ]; then fail insteadof-refused "refused only after fetching from the rewritten URL"
else pass insteadof-refused; fi

mkref "$tmp/branch"
g -C "$tmp/branch" checkout -q -b feature
g -C "$tmp/branch" update-ref refs/remotes/origin/feature HEAD
g -C "$tmp/branch" config branch.feature.remote origin
g -C "$tmp/branch" config branch.feature.merge refs/heads/feature
if KIMA_HUB_REFERENCE="$tmp/branch" KIMA_REFERENCE_COPY="$tmp/copy-br" prep --no-pull >/dev/null 2>&1; then
  fail non-main-refused "a checkout tracking a non-main upstream branch was accepted"
else pass non-main-refused; fi

mkref "$tmp/detached"
g -C "$tmp/detached" checkout -q --detach
if KIMA_HUB_REFERENCE="$tmp/detached" KIMA_REFERENCE_COPY="$tmp/copy-det" prep --no-pull >/dev/null 2>&1; then
  fail detached-refused "a detached HEAD was accepted"
else pass detached-refused; fi

# --- positive path + recorded provenance -------------------------------------------------------------------------
mkref "$tmp/ref"
sha="$(git -C "$tmp/ref" rev-parse HEAD)"
tree="$(git -C "$tmp/ref" rev-parse HEAD:frontend)"
out="$(KIMA_HUB_REFERENCE="$tmp/ref" KIMA_REFERENCE_COPY="$tmp/copy" prep --no-pull 2>"$tmp/err")"; rc=$?
if [ $rc -ne 0 ]; then fail provenance "clean canonical checkout refused (rc=$rc): $(cat "$tmp/err")"
else
  bad=""
  [ "$(val KIMA_HUB_REFERENCE_SHA "$out")" = "$sha" ] || bad+=" sha"
  [ "$(val KIMA_HUB_REFERENCE_REMOTE_URL "$out")" = "$CANON" ] || bad+=" remote-url"
  [ "$(val KIMA_HUB_REFERENCE_BRANCH "$out")" = main ] || bad+=" branch"
  [ "$(val KIMA_HUB_REFERENCE_UPSTREAM "$out")" = origin/main ] || bad+=" upstream"
  [ "$(val KIMA_HUB_REFERENCE_PULLED "$out")" = no ] || bad+=" pulled"
  [ -n "$(val KIMA_HUB_REFERENCE_RELATION "$out")" ] || bad+=" relation"
  [ "$(val KIMA_HUB_REFERENCE_TREE "$out")" = "$tree" ] || bad+=" tree"
  [[ "$(val KIMA_REFERENCE_MANIFEST "$out")" =~ ^[0-9a-f]{64}$ ]] || bad+=" manifest"
  [ "$(val KIMA_REFERENCE_COPY_RESTORED "$out")" = new ] || bad+=" restored"
  [ "$(val KIMA_HUB_REFERENCE_REMOTE_URL "$(cat "$tmp/copy/REFERENCE.env" 2>/dev/null)")" = "$CANON" ] || bad+=" reference.env"
  [ -z "$bad" ] && pass provenance || fail provenance "missing/wrong:$bad"
fi
manifest="$(val KIMA_REFERENCE_MANIFEST "$out")"

# Inherited GIT_DIR / GIT_WORK_TREE must not redirect the reference git calls.
mkref "$tmp/other"
echo other >>"$tmp/other/README.md"; g -C "$tmp/other" commit -qam other
g -C "$tmp/other" update-ref refs/remotes/origin/main HEAD
out2="$(GIT_DIR="$tmp/other/.git" GIT_WORK_TREE="$tmp/other" KIMA_HUB_REFERENCE="$tmp/ref" KIMA_REFERENCE_COPY="$tmp/copy" \
  prep --no-pull --check 2>/dev/null)"
[ "$(val KIMA_HUB_REFERENCE_SHA "$out2")" = "$sha" ] && pass git-env-scrubbed \
  || fail git-env-scrubbed "GIT_DIR redirected the reference to $(val KIMA_HUB_REFERENCE_SHA "$out2")"

# --- Important 1: same SHA, modified copy -> restored from git objects, node_modules kept, restart signalled -----
mkdir -p "$tmp/copy/frontend/node_modules/pkg" "$tmp/copy/frontend/.next"
echo keep >"$tmp/copy/frontend/node_modules/pkg/index.js"
echo cache >"$tmp/copy/frontend/.next/cache.txt"
out3="$(KIMA_HUB_REFERENCE="$tmp/ref" KIMA_REFERENCE_COPY="$tmp/copy" prep --no-pull 2>/dev/null)"
[ "$(val KIMA_REFERENCE_COPY_RESTORED "$out3")" = no ] && [ "$(val KIMA_REFERENCE_MANIFEST "$out3")" = "$manifest" ] \
  && pass unchanged-copy-kept || fail unchanged-copy-kept "an unmodified copy was reported as restored/changed"
[ -f "$tmp/copy/frontend/.next/cache.txt" ] && pass next-cache-kept-when-clean || fail next-cache-kept-when-clean ".next removed from a verified copy"
printf 'export const a = 2; // edited in the copy\n' >"$tmp/copy/frontend/lib/a.ts"
echo stray >"$tmp/copy/frontend/stray.ts"
out4="$(KIMA_HUB_REFERENCE="$tmp/ref" KIMA_REFERENCE_COPY="$tmp/copy" prep --no-pull 2>/dev/null)"
bad=""
[ "$(cat "$tmp/copy/frontend/lib/a.ts" 2>/dev/null)" = 'export const a = 1;' ] || bad+=" edit-not-restored"
[ ! -e "$tmp/copy/frontend/stray.ts" ] || bad+=" stray-file-kept"
[ "$(cat "$tmp/copy/frontend/node_modules/pkg/index.js" 2>/dev/null)" = keep ] || bad+=" node_modules-lost"
[ "$(val KIMA_REFERENCE_COPY_RESTORED "$out4")" = yes ] || bad+=" restored-flag"
[ "$(val KIMA_REFERENCE_MANIFEST "$out4")" = "$manifest" ] || bad+=" manifest-differs-after-restore"
[ -z "$bad" ] && pass dirty-copy-restored || fail dirty-copy-restored "$bad"
# A copy whose REFERENCE.env claims another SHA is also re-extracted, never trusted.
sed -i 's/^KIMA_HUB_REFERENCE_SHA=.*/KIMA_HUB_REFERENCE_SHA=0000000000000000000000000000000000000000/' "$tmp/copy/REFERENCE.env"
out5="$(KIMA_HUB_REFERENCE="$tmp/ref" KIMA_REFERENCE_COPY="$tmp/copy" prep --no-pull 2>/dev/null)"
[ "$(val KIMA_HUB_REFERENCE_SHA "$(cat "$tmp/copy/REFERENCE.env")")" = "$sha" ] && pass reference-env-rewritten \
  || fail reference-env-rewritten "stale REFERENCE.env kept"

# --- run-kima-web-visual.sh: when is the reference dev server restarted? -------------------------------------
if source <(sed -n '/^reference_restart_reason() {/,/^}/p' "$root/tools/run-kima-web-visual.sh") 2>/dev/null \
  && declare -F reference_restart_reason >/dev/null; then
  r() { reference_restart_reason "$@"; }
  bad=""
  [ -z "$(r s1 /c/frontend m1 s1 /c/frontend m1 no)" ] || bad+=" restart-without-change"
  [ -n "$(r s1 /c/frontend m1 s2 /c/frontend m1 no)" ] || bad+=" sha"
  [ -n "$(r s1 /c/frontend m1 s1 /d/frontend m1 no)" ] || bad+=" path"
  [ -n "$(r s1 /c/frontend m1 s1 /c/frontend m2 no)" ] || bad+=" manifest"
  [ -n "$(r s1 /c/frontend m1 s1 /c/frontend m1 yes)" ] || bad+=" restored"
  [ -n "$(r '' '' '' s1 /c/frontend m1 no)" ] || bad+=" unknown-previous-state"
  [ -z "$bad" ] && pass restart-decision || fail restart-decision "no restart on:$bad"
else fail restart-decision "run-kima-web-visual.sh has no reference_restart_reason()"; fi

if source <(sed -n '/^reference_next_bin() {/,/^}/p' "$root/tools/run-kima-web-visual.sh") 2>/dev/null \
  && declare -F reference_next_bin >/dev/null; then
  bad=""
  reference_next_bin "$tmp/nowhere" >/dev/null 2>&1 && bad+=" missing-dir"
  mkdir -p "$tmp/fe/node_modules/.bin"
  reference_next_bin "$tmp/fe" >/dev/null 2>&1 && bad+=" missing-next"
  printf '#!/bin/sh\n' >"$tmp/fe/node_modules/.bin/next"; chmod +x "$tmp/fe/node_modules/.bin/next"
  [ "$(reference_next_bin "$tmp/fe" 2>/dev/null)" = "$tmp/fe/node_modules/.bin/next" ] || bad+=" local-bin"
  [ -z "$bad" ] && pass local-next-only || fail local-next-only "$bad"
else fail local-next-only "run-kima-web-visual.sh has no reference_next_bin()"; fi
grep -q 'npx next' "$root/tools/run-kima-web-visual.sh" && fail no-npx "run-kima-web-visual.sh still runs npx next" || pass no-npx

# --- compare-kima-web-visual.mjs: the report's reference provenance fails closed -----------------------------
if node "$root/tools/compare-kima-web-visual.mjs" --check-provenance >/dev/null 2>&1; then
  fail compare-fail-closed "compare accepted a run without reference provenance"
else pass compare-fail-closed; fi
printf '%s\n' "$out" >"$tmp/prov.env"
json="$(KIMA_REFERENCE_PROVENANCE="$tmp/prov.env" node "$root/tools/compare-kima-web-visual.mjs" --check-provenance 2>"$tmp/err")"
if node -e 'const r=JSON.parse(process.argv[1]);process.exit(r.sha&&r.remoteUrl&&r.branch&&r.pulled&&r.relation&&r.tree&&r.manifest?0:1)' "$json" 2>/dev/null; then
  pass compare-provenance
else fail compare-provenance "report reference lacks sha/remoteUrl/branch/pulled/relation/tree/manifest: $json $(cat "$tmp/err")"; fi
sed 's#^KIMA_HUB_REFERENCE_REMOTE_URL=.*#KIMA_HUB_REFERENCE_REMOTE_URL=https://github.com/someone-else/kima-hub.git#' "$tmp/prov.env" >"$tmp/prov-fork.env"
KIMA_REFERENCE_PROVENANCE="$tmp/prov-fork.env" node "$root/tools/compare-kima-web-visual.mjs" --check-provenance >/dev/null 2>&1 \
  && fail compare-rejects-fork "fork provenance accepted" || pass compare-rejects-fork

# --- tools-env.inc.sh: only the six documented keys, and the environment (even empty) wins -------------------
cat >"$tmp/tools.env" <<EOF
LD_PRELOAD=$tmp/evil.so
BASH_ENV=$tmp/evil.sh
NODE_OPTIONS=--require=$tmp/evil.js
GIT_DIR=$tmp/other/.git
npm_config_registry=http://127.0.0.1:9/
KIMA_HUB_REFERENCE=$tmp/from-config
KIMA_CSS_DEPS=$tmp/css-from-config
EOF
res="$(env -i PATH="$PATH" HOME="$HOME" KIMA_RS_TOOLS_ENV="$tmp/tools.env" KIMA_HUB_REFERENCE= bash -c \
  '. "$1"; for k in LD_PRELOAD BASH_ENV NODE_OPTIONS GIT_DIR npm_config_registry; do [ -n "${!k+x}" ] && echo "leak:$k"; done
   echo "ref=[${KIMA_HUB_REFERENCE-unset}] css=[${KIMA_CSS_DEPS-unset}]"' _ "$root/tools/tools-env.inc.sh" 2>&1)"
if grep -q '^leak:' <<<"$res"; then fail tools-env-allow-list "$(grep '^leak:' <<<"$res" | tr '\n' ' ')"; else pass tools-env-allow-list; fi
grep -q "ref=\[\] css=\[$tmp/css-from-config\]" <<<"$res" && pass tools-env-empty-env-wins \
  || fail tools-env-empty-env-wins "$(grep '^ref=' <<<"$res")"

# --- gen-kima-web-css.mjs: exact direct versions, no parent node_modules -------------------------------------
fakepkg() { mkdir -p "$1/node_modules/$2"; printf '{"name":"%s","version":"%s","main":"index.js"}\n' "$2" "$3" >"$1/node_modules/$2/package.json"; echo 'module.exports = {};' >"$1/node_modules/$2/index.js"; }
deps="$tmp/css/deps"; mkdir -p "$deps"
fakepkg "$deps" tailwindcss 4.2.1; fakepkg "$deps" @tailwindcss/postcss 4.2.0; fakepkg "$deps" postcss 8.5.23
KIMA_CSS_DEPS="$deps" node "$root/tools/gen-kima-web-css.mjs" --check >"$tmp/out" 2>&1; rc=$?
{ [ $rc -eq 2 ] && grep -q '4.2.0' "$tmp/out"; } && pass css-exact-version || fail css-exact-version "rc=$rc $(head -c 300 "$tmp/out")"
deps2="$tmp/css/parent/deps"; mkdir -p "$deps2"
fakepkg "$deps2" tailwindcss 4.2.1; fakepkg "$deps2" @tailwindcss/postcss 4.2.1; fakepkg "$tmp/css/parent" postcss 8.5.23
KIMA_CSS_DEPS="$deps2" node "$root/tools/gen-kima-web-css.mjs" --check >"$tmp/out" 2>&1; rc=$?
{ [ $rc -eq 2 ] && grep -q 'postcss' "$tmp/out"; } && pass css-no-parent-resolution || fail css-no-parent-resolution "rc=$rc $(head -c 300 "$tmp/out")"

# --- gen-kima-web-icons.mjs: reads the tools config like the other tools, records the lucide version/source ---
fe="$tmp/iconcopy/frontend"; mkdir -p "$fe/node_modules/lucide-react/dist/esm/icons"
printf '{"name":"lucide-react","version":"0.552.0"}\n' >"$fe/node_modules/lucide-react/package.json"
printf '{"lockfileVersion":3,"packages":{"node_modules/lucide-react":{"version":"0.552.0","resolved":"https://registry.npmjs.org/lucide-react/-/lucide-react-0.552.0.tgz","integrity":"sha512-test"}}}\n' >"$fe/package-lock.json"
printf 'KIMA_REFERENCE_COPY=%s\n' "$tmp/iconcopy" >"$tmp/icons.env"
o="$(KIMA_RS_TOOLS_ENV="$tmp/icons.env" node "$root/tools/gen-kima-web-icons.mjs" --source 2>&1)"; rc=$?
{ [ $rc -eq 0 ] && grep -q '0.552.0' <<<"$o" && grep -q 'sha512-test' <<<"$o" && grep -q "$tmp/iconcopy/frontend" <<<"$o"; } \
  && pass icons-config-and-version || fail icons-config-and-version "rc=$rc $(head -c 300 <<<"$o")"
printf '{"name":"lucide-react","version":"0.553.0"}\n' >"$fe/node_modules/lucide-react/package.json"
KIMA_RS_TOOLS_ENV="$tmp/icons.env" node "$root/tools/gen-kima-web-icons.mjs" --source >/dev/null 2>&1 \
  && fail icons-lock-mismatch "installed lucide-react differing from the lockfile accepted" || pass icons-lock-mismatch
grep -q '^// Generated by tools/gen-kima-web-icons.mjs from lucide-react 0\.552\.0 ' "$root/crates/kima-web/src/icons_data.rs" \
  && pass icons-header-version || fail icons-header-version "checked-in icons_data.rs does not record the lucide version"

# --- vibe-goldens: upstream sources come from the PIN's git objects, never a working tree ----------------------
mkref "$tmp/goldens"
mkdir -p "$tmp/goldens/backend/src/services/matching"; echo 'export const x = 1;' >"$tmp/goldens/backend/src/services/matching/formula.ts"
if [ -f "$root/tools/vibe-goldens/upstream.mjs" ]; then
  o="$(UPSTREAM="$tmp/goldens" node --input-type=module -e '
    const m = await import(process.argv[1]);
    try { m.readPinned("backend/src/services/matching/formula.ts"); console.log("read-working-tree"); }
    catch (e) { console.log("refused:" + e.message); }' "$root/tools/vibe-goldens/upstream.mjs" 2>&1)"
  grep -q '^refused:' <<<"$o" && pass goldens-pin-objects || fail goldens-pin-objects "$o"
else fail goldens-pin-objects "tools/vibe-goldens/upstream.mjs missing"; fi
if grep -q 'kima-hub-latest\|readFileSync(path.join(src' "$root"/tools/vibe-goldens/*.mjs; then
  fail goldens-no-working-tree "a golden/bench script still reads a working tree or the pruned scratch default"
else pass goldens-no-working-tree; fi

echo "failures: $fails"
[ "$fails" -eq 0 ]
