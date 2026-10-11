# Tooling-only container: public sources are mounted separately at /work.
FROM --platform=linux/amd64 rust@sha256:408fe88047cef61a2087653b0c5255fa51c0f2d6d94ddedd7a2562a9b91a46f6
# Debian package repositories are live. A fresh build may change these tools;
# replay records installed versions and image ID instead of claiming full OS pinning.
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3-venv pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN python3 -m venv /opt/m0-venv \
    && /opt/m0-venv/bin/pip install --disable-pip-version-check cmake==3.31.6
CMD ["bash"]
