FROM ghcr.io/terrapkg/builder:f44 AS base

RUN --mount=type=cache,target=/var/cache \
    dnf install -y \
    xorriso \
    rpm \
    limine \
    systemd \
    skopeo \
    btrfs-progs \
    e2fsprogs \
    xfsprogs \
    dosfstools \
    grub2 \
    parted \
    gdisk \
    util-linux-core \
    grub2-efi \
    bootupd \
    grub2-tools-extra \
    uboot-images-armv8 \
    uboot-tools \
    rustc \
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
    cpio \
    bootc \
    fuse-overlayfs \
    coreutils \
    psmisc \
    openssl-devel \
    pkgconf \
    https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-44.noarch.rpm \
    https://mirrors.rpmfusion.org/nonfree/fedora/rpmfusion-nonfree-release-44.noarch.rpm
# Notes on the less obvious packages above:
#   cpio           builds the initramfs integration archive appended to dracut's output.
#   bootc          the `unified` layout drives `bootc install` at build time.
#   fuse-overlayfs fallback when the kernel refuses a composefs mount.
#   coreutils      `numfmt` formats ISO sizes in CI job summaries.
#   psmisc         process helpers used while polling builds.
#   openssl-devel, pkgconf
#                  linking `composefs-rs` pulls in `openssl-sys`, which needs the
#                  OpenSSL headers at build time and probes for them via pkg-config.
#   shim-x64, grub2-pc-modules, qemu-user-static-aarch64
#                  x86_64-only, so deliberately absent from this shared list. See
#                  the architecture-scoped installs below.
#   bootupd
#                  `grub2-efi` alone does not pull in shim. The ISO's
#                  removable-media loader needs it, `bootupctl` populates the
#                  `/usr/lib/efi/<component>/<version>/` cache katsu reads the EFI
#                  payload from (that directory is generated, not packaged), and
#                  `grub2-pc-modules` supplies the 512-byte hybrid MBR stub for
#                  BIOS hybrid boot.
# Keep this list comment-free inline: a `#` inside a backslash-continued command is
# not a shell comment, it becomes an argument.
# TODO: Probably don't add RPMFusion repos to the image, guide users to add GPG keys and repos themselves?

# Architecture-scoped packages, kept out of the list above so an ARM build does
# not try to resolve x86_64-exclusive names. `shim-x64` is a noarch wrapper that
# Requires `shim-x64` proper, and `qemu-user-static-aarch64` only exists on
# x86_64; either one fails the whole transaction on aarch64.
ARG TARGETARCH
RUN set -eux; \
    case "${TARGETARCH}" in \
      amd64) arch_pkgs="shim-x64 grub2-pc-modules qemu-user-static-aarch64" ;; \
      arm64) arch_pkgs="qemu-user-static" ;; \
      *) echo >&2 "Unsupported TARGETARCH: ${TARGETARCH}"; exit 1 ;; \
    esac; \
    dnf install -y --setopt=install_weak_deps=False ${arch_pkgs}

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
