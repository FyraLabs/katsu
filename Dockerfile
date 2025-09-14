FROM ghcr.io/terrapkg/builder:f42

RUN dnf install -y \
    xorriso \
    rpm \
    limine \
    rEFInd \
    systemd \
    btrfs-progs \
    e2fsprogs \
    xfsprogs \
    dosfstools \
    grub2 \
    parted \
    util-linux-core \
    systemd-container \
    grub2-efi \
    uboot-images-armv8 \
    uboot-tools \
    rustc \
    cargo

COPY . /src
WORKDIR /src

RUN cargo build

RUN cp /src/target/debug/katsu /usr/bin/katsu

WORKDIR /src/tests

CMD ["/usr/bin/bash"]

