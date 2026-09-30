# Source-free tooling preparation only; never COPY app/source or credentials.
# Public native AMD64 Rust image audited on nynuc on 2026-09-30.
FROM rust@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang cmake nasm ninja-build pkg-config time \
    && dpkg-query -W > /usr/local/share/p02-tooling-packages.txt \
    && rustc +1.97.1 --version > /usr/local/share/p02-toolchain.txt \
    && rm -rf /var/lib/apt/lists/*
