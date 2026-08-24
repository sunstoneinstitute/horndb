#!/usr/bin/env bash
#
# Re-vendor a trimmed SuiteSparse:GraphBLAS from an upstream tag.
#
#   crates/closure/vendor/refresh-graphblas.sh v10.3.0 [--skip-verify]
#
# Deletes vendor/GraphBLAS/ and re-copies it from a fresh shallow clone of the
# tag, keeping only the paths listed in graphblas-keep.txt. Nothing is patched:
# the vendored tree is a byte-exact subset of the upstream tag, so re-running
# with the same tag leaves `git status` clean. That is the drift check.
#
# The last step builds the crate for real (cargo, default features, which runs
# crates/closure/build.rs), so the cmake flags live in exactly one place.

set -euo pipefail

readonly UPSTREAM_URL="https://github.com/DrTimothyAldenDavis/GraphBLAS.git"

usage() {
    echo "usage: $0 <upstream-tag> [--skip-verify]" >&2
    echo "example: $0 v10.3.0" >&2
    exit 2
}

TAG=""
SKIP_VERIFY=0
for arg in "$@"; do
    case "$arg" in
        --skip-verify) SKIP_VERIFY=1 ;;
        -h|--help) usage ;;
        -*) echo "$0: unknown option: $arg" >&2; usage ;;
        *) [ -n "$TAG" ] && { echo "$0: more than one tag given" >&2; usage; }; TAG="$arg" ;;
    esac
done
[ -n "$TAG" ] || usage

for tool in git cmake; do
    command -v "$tool" >/dev/null || { echo "$0: required tool not found: $tool" >&2; exit 1; }
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
readonly REPO_ROOT
readonly VENDOR_DIR="$REPO_ROOT/crates/closure/vendor"
readonly DEST="$VENDOR_DIR/GraphBLAS"
readonly KEEP_LIST="$VENDOR_DIR/graphblas-keep.txt"
readonly STAMP="$VENDOR_DIR/GraphBLAS.vendor.md"

[ -f "$KEEP_LIST" ] || { echo "$0: missing keep-list: $KEEP_LIST" >&2; exit 1; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/graphblas-vendor.XXXXXX")"
readonly TMP
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# --- 1. fetch the tag -------------------------------------------------------

echo "==> cloning $UPSTREAM_URL at $TAG"
git clone --depth 1 --branch "$TAG" --quiet "$UPSTREAM_URL" "$TMP/upstream"
UPSTREAM_SHA="$(git -C "$TMP/upstream" rev-parse HEAD)"
readonly UPSTREAM_SHA

# --- 2. prune ---------------------------------------------------------------

# Strip comments and blank lines from the keep-list.
grep -v '^\s*#' "$KEEP_LIST" | grep -v '^\s*$' > "$TMP/keep"
echo "==> copying $(wc -l < "$TMP/keep" | tr -d ' ') paths into ${DEST#"$REPO_ROOT"/}"

# Verify every kept path exists upstream before touching the destination: a
# renamed or removed directory should fail here, not halfway through a copy.
missing=0
while IFS= read -r path; do
    [ -e "$TMP/upstream/$path" ] || { echo "$0: not present in $TAG: $path" >&2; missing=1; }
done < "$TMP/keep"
[ "$missing" -eq 0 ] || { echo "$0: fix $KEEP_LIST and re-run" >&2; exit 1; }

# Plain `cp -R` rather than rsync: macOS ships openrsync, whose --files-from
# does not recurse into the listed directories, so an rsync-based copy silently
# produces a tree of empty dirs there.
rm -rf "$DEST"
mkdir -p "$DEST"
while IFS= read -r path; do
    mkdir -p "$DEST/$(dirname "$path")"
    cp -R "$TMP/upstream/$path" "$DEST/$path"
done < "$TMP/keep"

# --- 3. record provenance ---------------------------------------------------

# Same rule as crates/closure/build/shared.rs::parse_version — first line
# mentioning the field, first whitespace-separated number after it — so the
# script and build.rs always agree on the shared-build directory name.
version_cmake="$DEST/cmake_modules/GraphBLAS_version.cmake"
gb_version_field() {
    sed -n "s/.*$1[[:space:]]\{1,\}\([0-9]\{1,\}\).*/\1/p" "$version_cmake" | head -1
}
VERSION="$(gb_version_field GraphBLAS_VERSION_MAJOR).$(gb_version_field GraphBLAS_VERSION_MINOR).$(gb_version_field GraphBLAS_VERSION_SUB)"
readonly VERSION
case "$VERSION" in
    *.*.*) [ "${VERSION//[0-9.]/}" = "" ] || VERSION="" ;;
    *) VERSION="" ;;
esac
[ -n "$VERSION" ] || { echo "$0: could not parse a version from $version_cmake" >&2; exit 1; }

FILE_COUNT="$(find "$DEST" -type f | wc -l | tr -d ' ')"
TREE_SIZE="$(du -sh "$DEST" | cut -f1 | tr -d ' ')"

cat > "$STAMP" <<STAMP_EOF
# Vendored SuiteSparse:GraphBLAS

Do not edit anything under \`GraphBLAS/\`. It is a byte-exact subset of an
upstream tag, produced by \`refresh-graphblas.sh\`. To change what is kept,
edit \`graphblas-keep.txt\` and re-run the script.

| | |
|---|---|
| Upstream | $UPSTREAM_URL |
| Tag | \`$TAG\` |
| Commit | \`$UPSTREAM_SHA\` |
| GraphBLAS version | $VERSION |
| Vendored | $FILE_COUNT files, $TREE_SIZE |

## Upgrading

\`\`\`bash
crates/closure/vendor/refresh-graphblas.sh v10.4.0
git add -A crates/closure/vendor
git commit -m 'vendor: SuiteSparse:GraphBLAS v10.4.0'
\`\`\`

The script rebuilds the crate at the end, so a version that needs a directory
missing from \`graphblas-keep.txt\` fails the upgrade rather than someone
else's build. Add the path and re-run.

## Checking for drift

Nothing here is patched, so re-running with the tag recorded above must leave
the working tree unchanged:

\`\`\`bash
crates/closure/vendor/refresh-graphblas.sh $TAG --skip-verify
git diff --quiet -- crates/closure/vendor/GraphBLAS && echo "no drift"
\`\`\`
STAMP_EOF

echo "==> vendored GraphBLAS $VERSION ($FILE_COUNT files, $TREE_SIZE) from $TAG @ ${UPSTREAM_SHA:0:12}"

# --- 4. verify with the real build ------------------------------------------

if [ "$SKIP_VERIFY" -eq 1 ]; then
    echo "==> skipping verification (--skip-verify)"
else
    # The shared build is keyed on (target, version). Drop this version's dir so
    # a re-vendor at an unchanged version rebuilds instead of reusing .complete.
    rm -rf "$VENDOR_DIR/.shared-build"/*/"$VERSION"
    echo "==> building and testing horndb-closure against the vendored tree"
    if command -v cargo-nextest >/dev/null; then
        ( cd "$REPO_ROOT" && cargo nextest run -p horndb-closure )
    else
        ( cd "$REPO_ROOT" && cargo test -p horndb-closure )
    fi
fi

cat <<NEXT

Next:
  git add -A crates/closure/vendor
  git status --short crates/closure/vendor | head
NEXT
