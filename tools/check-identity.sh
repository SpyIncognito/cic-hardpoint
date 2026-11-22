#!/usr/bin/env bash
# Fails when a tracked file, or any commit's author, committer or message, carries an identity or
# internal detail that must not be published. Three checks:
#   1. the generic patterns in tools/identity-patterns.txt;
#   2. any email address other than the project's own;
#   3. the private list (names and hostnames), kept outside the repo so it is never published:
#      the first argument, else $HARDPOINT_IDENTITY_DENY. Without one, this check is skipped
#      and the script says so.
# Usage: tools/check-identity.sh [private-list]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
ALLOWED_EMAIL='spy.incognito.games@gmail.com'
ALLOWED_IDENTITY="Spy Incognito <$ALLOWED_EMAIL>"
EMAIL='[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}'
self='tools/identity-patterns.txt'
private="${1:-${HARDPOINT_IDENTITY_DENY:-}}"
found=0

patterns=$(mktemp)
trap 'rm -f "$patterns"' EXIT
grep -v -E '^\s*(#|$)' "$self" > "$patterns"
if [[ -n "$private" ]]; then
  [[ -r "$private" ]] || { echo "private list not readable: $private" >&2; exit 2; }
  grep -v -E '^\s*(#|$)' "$private" >> "$patterns"
else
  echo "note: no private list given (argument or HARDPOINT_IDENTITY_DENY); names were not checked"
fi

report() { echo "$1"; found=1; }

# Files: the pattern file itself is excluded, since it holds the patterns.
if hits=$(git grep -n -I -i -E -f "$patterns" -- . ":!$self"); then
  report "files match the deny list:"; echo "$hits"
fi
if hits=$(git grep -n -I -o -E "$EMAIL" -- . | grep -v -F ":$ALLOWED_EMAIL"); then
  report "files carry another email address:"; echo "$hits"
fi

# History: identities, then messages.
# GitHub's web editor commits as GitHub's noreply address; it is the only other committer
# allowed, and only when the author is the project's own identity.
if hits=$(git log --all --format='%h %an <%ae> | %cn <%ce>' | grep -v -F " $ALLOWED_IDENTITY | $ALLOWED_IDENTITY" | grep -v -F " $ALLOWED_IDENTITY | GitHub <noreply"'@'"github.com>"); then
  report "commits by another identity:"; echo "$hits"
fi
messages=$(git log --all --format='@@%h%n%B')
if hits=$(grep -n -i -E -f "$patterns" <<<"$messages"); then
  report "commit messages match the deny list:"; echo "$hits"
fi
if hits=$(grep -n -o -E "$EMAIL" <<<"$messages" | grep -v -F ":$ALLOWED_EMAIL"); then
  report "commit messages carry another email address:"; echo "$hits"
fi

if (( found )); then echo "identity check: FAILED"; exit 1; fi
echo "identity check: clean"
