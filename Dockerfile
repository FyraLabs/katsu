FROM ghcr.io/terrapkg/builder:f43 AS base

RUN --mount=type=cache,target=/var/cache \
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
    grub2-efi \
    uboot-images-armv8 \
    uboot-tools \
    rustc \
    qemu-user-static-aarch64 \
    qemu-user-binfmt \
    qemu-img \
    cargo \
    mkpasswd \
    clang-devel \
    squashfs-tools \
    erofs-utils \
    grub2-tools \
    grub2-tools-extra \
    rEFInd \
    rEFInd-tools \
    isomd5sum \
    dnf5 \
    setfiles \
    podman \
    https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-43.noarch.rpm \
    https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-43.noarch.rpm
# TODO: Probably don't add RPMFusion repos to the image, guide users to add GPG keys and repos themselves?

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

RUN dnf mark user -y zstd fedora-gpg-keys
RUN dnf remove -y \
    anda \
    mock \
    mold \
    gh \
    jq \
    subatomic-cli \
    gdb-minimal \
    *-srpm-macros \
    terra-mock-configs
RUN dnf clean all

COPY --from=rust-builder /usr/bin/katsu /usr/bin/katsu


# clean up unnecessary packages to reduce image size


ENTRYPOINT [ "katsu" ]
