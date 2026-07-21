# HornDB SPARQL server image.
#
# Ships the `serve` binary (horndb-sparql, `server` + `reasoner` features) as
# `horndb`. GraphBLAS is vendored and statically linked by the closure
# crate's build.rs; OpenMP (libgomp) is the only shared runtime dependency, so
# the runtime image installs `libgomp1`. FFI bindings are checked in, so no
# libclang is needed at build time.
#
# Build context must include the vendored GraphBLAS submodule
# (crates/closure/vendor/GraphBLAS) — CI checks out with submodules: recursive.

# ---- build stage ----
FROM rust:1.90-bookworm AS build

# cmake + a C/C++ toolchain build the vendored GraphBLAS; pkg-config resolves
# its static .pc. libgomp is pulled in transitively by gcc for the OpenMP link.
RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# Build only the server binary and its dependency graph.
RUN cargo build --release --locked -p horndb-sparql --bin serve --features server \
    && cp target/release/serve /horndb \
    && strip /horndb

# ---- runtime stage ----
FROM debian:bookworm-slim AS runtime

# libgomp1: OpenMP runtime for the statically-linked GraphBLAS. ca-certificates
# for outbound TLS (e.g. owl:imports resolution).
RUN apt-get update \
    && apt-get install -y --no-install-recommends libgomp1 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /horndb /usr/local/bin/horndb
COPY LICENSE /usr/local/share/horndb/LICENSE

# HornDB's standard port. Override the bind address at runtime with
# `--bind 0.0.0.0:3840` so the server is reachable from outside the container.
EXPOSE 3840

# `--data` is required; users mount their RDF and pass it, e.g.
#   docker run -p 3840:3840 -v $PWD/data:/data ghcr.io/sunstoneinstitute/horndb \
#     --data /data/graph.ttl --bind 0.0.0.0:3840
ENTRYPOINT ["horndb"]
