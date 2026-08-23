FROM rust:1.97.1-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97 AS build

ARG PLURX_BUILD_SHA
ENV PLURX_BUILD_SHA=${PLURX_BUILD_SHA}

WORKDIR /src
COPY . .
RUN cargo build --locked --release -p plurx-cluster-check

FROM debian:bookworm-slim@sha256:abd67ffcfa541b485a3dff59865ab629aa048a6c613e639d36e7456b0b229241

ARG PLURX_BUILD_SHA
LABEL org.opencontainers.image.revision=${PLURX_BUILD_SHA}

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/plurx-cluster-check /usr/local/bin/plurx-cluster-check
ENTRYPOINT ["/usr/local/bin/plurx-cluster-check"]
