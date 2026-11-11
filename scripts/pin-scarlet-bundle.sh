#!/usr/bin/env bash
# Run after release assets have been uploaded and verified. Only the generated
# manifest may move onto newer main commits; the release keeps its source SHA.
set -euo pipefail

source_revision="${1:?source revision required}"
version="${2:?release version required}"
bundle=platforms/scarlet/bundle.toml

git add -- "$bundle"
if git diff --cached --quiet; then
  exit 0
fi
git commit -m "Pin Scarlet app release $version" -- "$bundle"

for attempt in 1 2 3; do
  git fetch origin main
  if ! git merge-base --is-ancestor "$source_revision" origin/main; then
    echo "Release source is no longer an ancestor of main; refusing to update bundle." >&2
    exit 1
  fi
  # This checkout contains exactly one generated-manifest commit above the
  # release source. A previous release may have changed the same file while
  # this build ran; on rebase, 'theirs' is this verified manifest's version.
  git rebase -X theirs origin/main
  if git push origin HEAD:main; then
    exit 0
  fi
  echo "Main advanced while updating the bundle; retry $attempt/3." >&2
done
echo "Unable to update bundle after three non-forced pushes." >&2
exit 1
