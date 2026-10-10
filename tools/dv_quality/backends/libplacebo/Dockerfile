FROM rust@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends python3-venv python3-pip pkg-config ninja-build libvulkan-dev mesa-vulkan-drivers vulkan-tools glslang-dev spirv-tools liblcms2-dev libxxhash-dev libavutil-dev libavcodec-dev libavformat-dev ffmpeg && rm -rf /var/lib/apt/lists/*
RUN python3 -m venv /opt/m0/venv && /opt/m0/venv/bin/pip install --no-cache-dir meson==1.10.0 Jinja2==3.1.6 MarkupSafe==3.0.3
ENV PATH=/opt/m0/venv/bin:$PATH
RUN dpkg-query -W > /opt/m0/package-versions.txt
WORKDIR /work
