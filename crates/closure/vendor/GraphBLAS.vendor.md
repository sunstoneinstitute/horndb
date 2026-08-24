# Vendored SuiteSparse:GraphBLAS

Do not edit anything under `GraphBLAS/`. It is a byte-exact subset of an
upstream tag, produced by `refresh-graphblas.sh`. To change what is kept,
edit `graphblas-keep.txt` and re-run the script.

| | |
|---|---|
| Upstream | https://github.com/DrTimothyAldenDavis/GraphBLAS.git |
| Tag | `v10.3.0` |
| Commit | `2135315ec62aca4f7e4b477c0d96a859386eed92` |
| GraphBLAS version | 10.3.0 |
| Vendored | 3875 files, 43M |

## Upgrading

```bash
crates/closure/vendor/refresh-graphblas.sh v10.4.0
git add -A crates/closure/vendor
git commit -m 'vendor: SuiteSparse:GraphBLAS v10.4.0'
```

The script rebuilds the crate at the end, so a version that needs a directory
missing from `graphblas-keep.txt` fails the upgrade rather than someone
else's build. Add the path and re-run.

## Checking for drift

Nothing here is patched, so re-running with the tag recorded above must leave
the working tree unchanged:

```bash
crates/closure/vendor/refresh-graphblas.sh v10.3.0 --skip-verify
git diff --quiet -- crates/closure/vendor/GraphBLAS && echo "no drift"
```
