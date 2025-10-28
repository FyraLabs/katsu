FROM ghcr.io/terrapkg/builder:f43 AS base

RUN --mount=type=cache,target=/var/cache/dnf \
    dnf install -y \
    xorriso \
    rpm \
    limine \
    systemd \
    btrfs-progs \
    e2fsprogs \
    xfsprogs \
    dosfstools \
    grub2 \
    parted \
    gdisk \
    util-linux-core \
    systemd-container \
    grub2-efi \
    uboot-images-armv8 \
    uboot-tools \
    rustc \
    qemu-user-static-aarch64 \
    qemu-user-binfmt \
    qemu-kvm \
    qemu-img \
    cargo \
    systemd-devel \
    mkpasswd \
    clang-devel \
    moby-engine \
    squashfs-tools \
    erofs-utils \
    grub2-tools \
    grub2-tools-extra \
    rEFInd \
    rEFInd-tools \
    isomd5sum \
    dnf5 \
    podman

FROM base AS rust-builder

COPY . /src

WORKDIR /src

RUN --mount=type=cache,target=/src/target \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release && cp target/release/katsu /usr/bin/katsu

FROM base AS runtime

COPY --from=rust-builder /usr/bin/katsu /usr/bin/katsu

ENTRYPOINT [ "katsu" ]
