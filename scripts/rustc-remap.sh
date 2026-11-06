#!/bin/sh
# rustc wrapper: strip the checkout's absolute path out of build artifacts.
#
# Cargo runs rustc from the workspace root, so $PWD is the checkout path. Every
# worktree sits at a different one, which lands in DW_AT_comp_dir and makes
# otherwise-identical artifacts differ byte-for-byte. Remapping it to a fixed
# name makes builds reproducible across worktrees and machines.
#
# Cargo deliberately leaves --remap-path-prefix out of its fingerprint, so this
# does not force rebuilds or change artifact hashes.
#
# This script takes cargo's one `build.rustc-wrapper` slot, which hides a
# global `rustc-wrapper = "sccache"` in ~/.cargo/config.toml. So chain to
# sccache here whenever it is installed.
RUSTC="$1"
shift
if command -v sccache >/dev/null 2>&1; then
    exec sccache "$RUSTC" "$@" --remap-path-prefix "$PWD=/horndb"
fi
exec "$RUSTC" "$@" --remap-path-prefix "$PWD=/horndb"
