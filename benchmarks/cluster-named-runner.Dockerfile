FROM rust:1.97.1-bookworm AS build

WORKDIR /src
COPY . .
RUN cargo build --locked --release -p plurx-cluster-check

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/plurx-cluster-check /usr/local/bin/plurx-cluster-check
ENTRYPOINT ["/usr/local/bin/plurx-cluster-check"]
