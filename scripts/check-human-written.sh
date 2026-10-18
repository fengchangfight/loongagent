#!/usr/bin/env bash
#
# docs/human-written is the human's notebook, not a document the agent maintains.
# AGENTS.md states the rule; this script is what makes it fail when it is broken,
# and scripts/check.sh (the same set CI runs) calls it.
#
# A change is either:
#   * the working tree or an untracked file under docs/human-written — the
#     ordinary way an agent edits, caught even before a commit; or
#   * a commit in the checked range that touches the directory without the
#     human's `Human-Written: true` trailer.
#
# The human's two explicit escapes:
#   HUMAN_WRITTEN_ALLOW=1 ./scripts/check-human-written.sh   # this run only
#   git commit -m "..." -m "Human-Written: true"             # attest in the record
#
# CI sets HUMAN_WRITTEN_BASE to the pull request's base / the previous commit.
# Locally it defaults to HEAD, which still catches uncommitted agent edits.
set -euo pipefail

cd "$(dirname "$0")/.."

dir="docs/human-written"
base="${HUMAN_WRITTEN_BASE:-HEAD}"
allow="${HUMAN_WRITTEN_ALLOW:-0}"

if [ "$allow" = "1" ]; then
  echo "human-written: allowed for this run (HUMAN_WRITTEN_ALLOW=1)"
  exit 0
fi

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "human-written: not a git checkout; nothing to compare" >&2
  exit 0
fi

# A blank or zero base (first push, no pull-request context) means "judge the
# working tree", not "fail for a reason unrelated to the rule".
case "$base" in
  ""|0000000000000000000000000000000000000000) base="HEAD" ;;
esac
if ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
  for candidate in origin/main main HEAD~1; do
    if git cat-file -e "${candidate}^{commit}" 2>/dev/null; then
      base="$candidate"
      break
    fi
  done
fi
# Nothing usable: start from the root commit, so the committed range is still
# judged rather than silently passed because of a missing base.
if ! git cat-file -e "${base}^{commit}" 2>/dev/null; then
  base="$(git rev-list --max-parents=0 HEAD 2>/dev/null | tail -1)"
fi
[ -n "$base" ] || base="HEAD"

fail=0

tracked="$(git diff HEAD --name-only -- "$dir" || true)"
untracked="$(git ls-files --others --exclude-standard -- "$dir" || true)"
if [ -n "$tracked" ] || [ -n "$untracked" ]; then
  fail=1
  {
    echo "error: $dir is human-written and the working tree changes it:" >&2
    printf '%s\n' "$tracked" "$untracked" | sed '/^$/d' | sort -u | sed 's/^/  /' >&2
  } >&2
fi

# Every commit in range that touched the directory must carry the human's
# trailer. A commit without it is an agent edit that reached history.
while IFS= read -r commit; do
  [ -n "$commit" ] || continue
  if ! git log -1 --format=%B "$commit" | grep -qx 'Human-Written: true'; then
    fail=1
    echo "error: commit ${commit:0:12} touches $dir without 'Human-Written: true':" >&2
    echo "  $(git log -1 --format='%s' "$commit")" >&2
  fi
done < <(git log --format=%H "$base"..HEAD -- "$dir" 2>/dev/null || true)

if [ "$fail" -ne 0 ]; then
  cat >&2 <<EOF

$dir is input for the agent, never output. If a task needs a change there,
stop and ask the human to make it. The human can:
  - commit the note with a 'Human-Written: true' trailer, or
  - run HUMAN_WRITTEN_ALLOW=1 ./scripts/check-human-written.sh for one check.
EOF
  exit 1
fi

echo "human-written: untouched"
