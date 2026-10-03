#!/usr/bin/env bash
# Builds the Linux release tarball twice, from two different absolute
# paths, and checks the result is byte-identical. This is the local
# equivalent of the `reproducible` CI workflow for the one platform that's
# cheap to build twice on a single machine; see docs/RELEASING.md and
# .github/workflows/reproducible.yml for the Android side, which needs two
# separate runners to be a meaningful test.
#
#   tools/repro-check.sh [git ref, default HEAD]
#
# Needs ~6 GB of free disk (two full Linux release builds) and whatever
# `flutter build linux --release` itself needs (clang, cmake, ninja-build,
# pkg-config, libgtk-3-dev). Prints a pass/fail and removes its temporary
# worktrees and build output either way; nothing it creates is left behind
# on exit, including on failure.
set -euo pipefail

ref="${1:-HEAD}"
root="$(cd "$(dirname "$0")/.." && pwd)"

need_gb=6
avail_gb="$(df -Pk "$root" | awk 'NR==2 { print int($4/1024/1024) }')"
if (( avail_gb < need_gb )); then
  echo "error: need about ${need_gb} GB free under $root, only ${avail_gb} GB available." >&2
  echo "This builds the Linux release bundle twice; free up disk or run on a bigger machine." >&2
  exit 1
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/kn-repro-check.XXXXXX")"
cleanup() {
  # Remove the worktrees through git first so .git/worktrees doesn't keep
  # dangling references, then the scratch directory itself.
  git -C "$root" worktree remove --force "$work/a/src" >/dev/null 2>&1 || true
  git -C "$root" worktree remove --force "$work/b/src" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

commit="$(git -C "$root" rev-parse "$ref")"
source_date_epoch="$(git -C "$root" log -1 --format=%ct "$commit")"
echo "Building $commit twice, from two different paths, comparing the Linux tarball."
echo "SOURCE_DATE_EPOCH=$source_date_epoch (commit time, same for both builds)"

build_one() {
  local label="$1" dir="$2"
  # A deliberately different path depth and name between the two builds:
  # if the remap-path-prefix flags were wrong, only this would catch it.
  mkdir -p "$dir"
  git -C "$root" worktree add --detach --quiet "$dir/src" "$commit"

  echo "== [$label] flutter build linux --release in $dir/src"
  (
    cd "$dir/src/app"
    flutter pub get >/dev/null
    export CARGO_HOME="$dir/cargo-home"
    export CARGO_INCREMENTAL=0
    export RUSTFLAGS="--remap-path-prefix=$dir/src=/build --remap-path-prefix=$dir/cargo-home=/cargo-home"
    flutter build linux --release >/dev/null
  )

  local version="0.0.0-repro-check"
  mkdir -p "$dir/dist"
  tar -C "$dir/src/app/build/linux/x64/release" \
    --sort=name --mtime="@$source_date_epoch" --owner=0 --group=0 --numeric-owner \
    --transform "s,^bundle,kilonova-$version," \
    -cf - bundle | gzip -n -9 > "$dir/dist/kilonova-$version-linux-x64.tar.gz"
}

build_one "A" "$work/a"
build_one "B" "$work/b"

tar_a="$work/a/dist/kilonova-0.0.0-repro-check-linux-x64.tar.gz"
tar_b="$work/b/dist/kilonova-0.0.0-repro-check-linux-x64.tar.gz"
sha_a="$(sha256sum "$tar_a" | cut -d' ' -f1)"
sha_b="$(sha256sum "$tar_b" | cut -d' ' -f1)"

echo
echo "A: $sha_a  $tar_a"
echo "B: $sha_b  $tar_b"

if [[ "$sha_a" == "$sha_b" ]]; then
  echo
  echo "REPRODUCIBLE: both builds of the Linux tarball are byte-identical."
  exit 0
fi

echo
echo "NOT REPRODUCIBLE: the two builds differ." >&2
if command -v diffoscope >/dev/null; then
  echo "Running diffoscope for a detailed diff (this can take a while):"
  diffoscope "$tar_a" "$tar_b" || true
else
  echo "diffoscope is not installed; falling back to a per-file sha256 comparison." >&2
  echo "Install diffoscope for a readable diff of what actually changed." >&2
  diff_dir="$work/extract"
  mkdir -p "$diff_dir/a" "$diff_dir/b"
  tar -xzf "$tar_a" -C "$diff_dir/a"
  tar -xzf "$tar_b" -C "$diff_dir/b"
  ( cd "$diff_dir/a" && find . -type f -exec sha256sum {} + | sort ) > "$work/a.sha256"
  ( cd "$diff_dir/b" && find . -type f -exec sha256sum {} + | sort ) > "$work/b.sha256"
  diff -u "$work/a.sha256" "$work/b.sha256" || true
fi
exit 1
