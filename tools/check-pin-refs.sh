#!/usr/bin/env bash
# Fails when the old upstream pin (12faf1cd, v1.11.0) is named outside the
# places where it is history or provenance. See docs/DIVERGENCES.md (re-pin note).
set -euo pipefail
cd "$(dirname "$0")/.."
allowed='^(docs/DIVERGENCES\.md|docs/superpowers/plans/2026-11-05-m-sync\.md|docs/superpowers/specs/2026-11-03-m4-ml-analysis-design\.md):|^crates/kima-ml/|^tools/check-pin-refs\.sh:'
bad=$(grep -rIn '12faf1cd' . --exclude-dir=.git --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.next \
  | sed 's|^\./||' | grep -Ev "$allowed" \
  | grep -Ev '^docs/superpowers/plans/2026-10-16-m0-skeleton\.md:[0-9]+:git commit -m "chore\(frontend\): fork upstream frontend @ 12faf1cd"$' \
  | grep -Ev '^docs/superpowers/plans/2026-10-16-kima-rs-roadmap\.md:[0-9]+:.*old pin' || true)
if [ -n "$bad" ]; then echo "old pin referenced outside the allow-list:"; echo "$bad"; exit 1; fi
echo "pin refs ok"
