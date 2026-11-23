# fleet-sim as a container: demo fleet members for testing a CIC relay. Not Hardpoint itself,
# which ships as a Windows exe. This is the root Dockerfile because the lab deploy server builds
# a repository's root Dockerfile only; tools/fleet-sim/Dockerfile links here.
# Build from the repository root: docker build -t fleet-sim .
#
# Run: recordings (scrubbed .jsonl, read at start, never built in) mounted at /recordings, the
# members' keys and files at /data (keep it, so a restart rejoins by key). For a CIC relay, mount
# a file of connect strings read-only and name it in FLEET_SIM_CONNECT_FILE; for peer to peer, set
# FLEET_SIM_INVITE. Without either it waits.

FROM rust:1.100.0-trixie AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY .cargo .cargo
COPY crates crates
COPY tools/fleet-sim tools/fleet-sim
# Built into the crates fleet-sim uses: the telemetry schema (sc-game) and the pages (sc-service).
COPY schema schema
COPY pages pages
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p fleet-sim \
    && cp target/release/fleet-sim /fleet-sim

# The runtime is the same Debian release as the builder, so the glibc matches.
FROM debian:trixie-20261104-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --home-dir /nonexistent \
        --shell /usr/sbin/nologin fleetsim \
    && mkdir -p /data /recordings \
    && chown fleetsim:fleetsim /data \
    && chmod 700 /data
COPY --from=builder /fleet-sim /usr/local/bin/fleet-sim

ENV FLEET_SIM_RECORDINGS=/recordings \
    FLEET_SIM_DATA=/data \
    FLEET_SIM_COUNT=8 \
    RUST_LOG=info,sc_service=warn,sc_core=warn,sc_game=warn

USER fleetsim
WORKDIR /
VOLUME ["/data"]
# SIGTERM disconnects every member without leaving the event.
STOPSIGNAL SIGTERM
ENTRYPOINT ["fleet-sim"]
CMD ["--wait"]
