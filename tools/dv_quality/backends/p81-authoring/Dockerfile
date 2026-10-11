# Same pinned Rust multiarch base as the approved GPU mechanics bundle.
FROM rust@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
# Debian repositories are live, not a fully pinned OS package snapshot.
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends python3 ffmpeg && rm -rf /var/lib/apt/lists/*
RUN dpkg-query -W > /opt/authoring-package-versions.txt
WORKDIR /work
