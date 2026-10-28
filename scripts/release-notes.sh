#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Print one release's entry from wiki/Release-Notes.md, without its heading:
#
#   scripts/release-notes.sh 1.2.0
#
# Exits 1 if there's no dated `## 1.2.0 — YYYY-MM-DD` heading, so a release
# can't be published before its notes are written and dated.
set -eu

version=${1:?usage: release-notes.sh <version>}
notes=${NOTES:-$(dirname "$0")/../wiki/Release-Notes.md}

entry=$(awk -v v="$version" '
  /^## / {
    if (found) exit
    if (index($0, "## " v " — ") == 1 && $0 ~ /— [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/) { found = 1; next }
  }
  found { print }
  END { if (!found) exit 1 }
' "$notes") || {
  echo "release-notes.sh: no dated \"## $version — YYYY-MM-DD\" entry in $notes" >&2
  exit 1
}
# without the blank lines around it
printf '%s\n' "$entry" | sed -e '/./,$!d'
