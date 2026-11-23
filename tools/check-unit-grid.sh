#!/usr/bin/env bash
# Checks that pages/unit-grid.js, less its header, is cic-panel's static/unit-grid.js at the
# commit the header names, and says whether that checkout's HEAD has changed the file since.
# Usage: tools/check-unit-grid.sh <cic-panel checkout>
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
copy="$root/pages/unit-grid.js"
src="crates/cic-relay/static/unit-grid.js"
cic="${1:?usage: tools/check-unit-grid.sh <cic-panel checkout>}"

commit="$(sed -n '1s/^\/\/ Copied from cic-panel [^ ]* at \([0-9a-f]\{7,40\}\);.*/\1/p' "$copy")"
[[ -n "$commit" ]] || { echo "pages/unit-grid.js names no cic-panel commit on its first line" >&2; exit 1; }
start="$(grep -n -m1 '^// unit-grid.js:' "$copy" | cut -d: -f1)"
[[ -n "$start" ]] || { echo "pages/unit-grid.js has no source text after its header" >&2; exit 1; }

diff <(git -C "$cic" show "$commit:$src") <(tail -n "+$start" "$copy")
echo "pages/unit-grid.js matches cic-panel $commit"
if git -C "$cic" merge-base --is-ancestor "$commit" HEAD && ! git -C "$cic" diff --quiet "$commit" HEAD -- "$src"; then
  echo "note: cic-panel HEAD ($(git -C "$cic" rev-parse --short HEAD)) has changed $src since $commit; copy it again"
fi
