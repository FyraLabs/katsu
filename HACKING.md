# Developing Katsu

Katsu is written in Rust, so to build it you'll need to have a Rust toolchain installed. We recommend using [rustup](https://rustup.rs/) to manage your Rust installation.

To build Katsu, simply clone the repository and run:

```bash
cargo build --release
```

This will produce a binary in the `target/release` directory.

## Running Katsu in a container

As of Katsu 0.10.2, Katsu now can be run inside an OCI container for easier sandboxing and CI/CD integration.

To run Katsu inside a container, you can use something like this:

```bash
podman run --rm -it \
    --privileged \ # required for loop device and mounting
    --cap-add=ALL \
    --security-opt seccomp=unconfined \
    --device /dev/loop-control \
    --device /dev/fuse \
    -v /dev:/dev:rw \
    -v ./:/workdir:Z \
    -w /workdir \
    ghcr.io/fyralabs/katsu:latest \
    katsu <args>
```

This will create a privileged container with access to loop devices and FUSE, which are required for Katsu to function properly.

We also provide a wrapper shell script [`scripts/katsupod`](./scripts/katsupod) to simplify this process.

If you would like to still run Katsu in a rootless sandboxed environment, you may use [Podman Machines](https://docs.podman.io/en/v5.2.2/markdown/podman-machine.1.html) to create a VM that can run Katsu without actually requiring root privileges on the host system.

Note that EROFS image creation may consume significant amounts of memory and CPU, so ensure that your container or VM has sufficient resources allocated.

```bash
# Create Podman machine with 8 vCPUs and 8GB (8192MiB) RAM, and start it immediately
podman machine init --cpus=8 --memory=8192 --rootful --now

# ..or, if you already have a Podman Machine, you can bump its resources to meet Katsu's requirements
podman machine set --cpus=8 --memory=8192 --rootful
podman machine start

```

This also means you can now hack on Katsu directly from unsupported platforms like macOS and Windows by using Podman Machines as your development environment!

## Image layouts

`bootc.layout` selects how the image is placed on the media:

| Layout | Boot mechanism | Offline install |
|---|---|---|
| `ostree` (default) | `ostree=` + `ostree-prepare-root` | No local source |
| `nested` (legacy) | image embedded in the chroot's own store | via that store |
| `unified` | `composefs=` + `bootc-root-setup` | Yes, read-only image store |

`ostree` imports the image through `ostree container image pull` into an OSTree
sysroot the media boots directly, so no podman or skopeo is needed at runtime.
The import preserves the manifest, image configuration, digest and layer refs
that `bootc status` reads, and deploys with an `origin.container-image-reference`
using the unverified policy (no signature verification is claimed).

`unified` builds bootc's native composefs layout. The ISO carries one read-only
`LiveOS/rootfs.img`. At boot the initramfs mounts the media by label, loop-mounts
the payload and exposes it at `/sysroot` as a **bind mount**; `bootc-root-setup`
then mounts the composefs image itself and assembles the root.

Two filesystem details are load-bearing:

- `/sysroot` must not be an OverlayFS. The kernel refuses an EROFS image whose
  backing file lives on overlayfs (`ENOTBLK`), and the composefs repository lives
  inside that image, so a bind mount of the loop payload is used instead.
- The read-only payload needs a writable deployment state. `state/deploy/<D>/{etc,var}`
  is overlaid with a tmpfs upper before `bootc-root-setup` runs, so services that
  write under `/var` (logind, NetworkManager, sshd, homed) can start.

The payload contains a read-only containers-storage as well, so the live system can
install itself offline:

```sh
bootc install to-disk \
  --source-imgref containers-storage:<image> \
  --target-imgref <registry image> /dev/vdb
```

**Image requirement:** `additionalimagestores` must include
`/usr/lib/bootc/storage` in the image's full `/etc/containers/storage.conf`. The
shipped `/usr/share/containers/storage.conf` points only at the empty
`/usr/lib/containers/storage`, and `storage.conf.d` drop-ins are not honoured for
this setting.

The `unified` layout stages a disk image because bootc's composefs installer
requires a real backing device with an ESP. Set its size with
`bootc.staging_disk_size` (default `24G`) when an image is larger than that; the
file is sparse, but it must fit the composefs objects and the imported image
store, which are written separately before dedup.

### Building

```sh
cargo build
sudo env KATSU_LOG=info target/debug/katsu -o iso tests/ng/bootc/katsu-iso-bootc.yaml
```

After a complete root build, iterate on later phases without rebuilding the
repository or root image:

```sh
sudo env KATSU_LOG=info target/debug/katsu -o iso \
  --skip-phases=root,rootimg tests/ng/bootc/katsu-iso-bootc.yaml
```

The existing payload is renamed from `squashfs.img` to `rootfs.img` if needed,
without copying it. Do not skip `dracut` when changing the initramfs integration;
`cpio` must be installed on the build host.

Boot the result under UEFI/OVMF with `-serial mon:stdio -no-reboot`. For serial
debugging use `console=ttyS0,115200 rd.debug rd.shell panic=0`; do not use
`rd.emergency=reboot`, which reboots instead of giving a shell.

### EROFS sizing

Shipped media favor size over build time:

```text
-E all-fragments,fragdedupe=inode,dedupe   --workers=2
```

`fragdedupe` accepts only `inode` or `full` (`full` compares every fragment's
content, and is heavier still). `dedupe` dedupes compressed data globally and is
**single-threaded by design** — `mkfs.erofs` warns "multi-threaded dedupe is NOT
implemented" — so `--workers` does not bound its memory, which scales with the
tree. The worker count is therefore capped, and `dedupe` can be dropped entirely
on a host without headroom.

Every `mkfs.erofs` option the build needs is exposed as a feature flag:

| Flag | Effect |
|---|---|
| `erofs-compression=<spec>` | `-z`, e.g. `zstd,level=6`, `lzma,6`, `xz` |
| `erofs-chunk-size=<n>` | `-C` physical cluster size |
| `erofs-xattr-level=<n>` | `-x` xattr level |
| `erofs-workers=<n>` | `--workers`, overrides the dedupe cap |
| `erofs-fragdedupe=inode\|full` | `fragdedupe` mode (default `inode`) |
| `erofs-fragments=none\|plain\|all` | `none`, `fragments`, or `all-fragments` |
| `no-erofs-dedupe` | drop `-E dedupe` |

Invalid values fail loudly rather than being silently ignored, since a typo would
otherwise produce an image built with the wrong settings and no indication.

The compression-test CI job drives these across a matrix and reports size and
build time to the run's job summary.

Unified storage shares data between the composefs object store and the image
store via reflinks, which EROFS cannot see: content dedup recovers what is
byte-identical, but not the extent sharing itself. Measured, the unified tree
still compresses to less than the OSTree layout's ISO.

## Contributing

We welcome contributions to Katsu! Whether you're fixing bugs, adding features, improving documentation, or reporting issues, your help is appreciated.

### Getting Started

1. **Fork the repository** on GitHub
2. **Clone your fork** locally:

   ```bash
   git clone https://github.com/YOUR_USERNAME/katsu.git
   cd katsu
   ```

3. **Create a new branch** for your changes:

   ```bash
   git checkout -b feature/your-feature-name
   ```

### Development Workflow

1. **Make your changes** following the project's coding style
2. **Test your changes** thoroughly:

   ```bash
   # Build the project
   cargo build
   
   # Run tests
   cargo test
   
   # Check for linting issues
   cargo clippy -- -D warnings
   
   # Format your code
   cargo fmt
   ```

3. **Commit your changes** with clear, descriptive commit messages:

   ```bash
   git commit -m "feat: add support for XYZ"
   ```

4. **Push to your fork**:

   ```bash
   git push origin feature/your-feature-name
   ```

5. **Open a Pull Request** on the main repository

### Code Style

Katsu uses the standard Rust formatting conventions. Please ensure your code is formatted with `cargo fmt` before submitting. The project includes a `rustfmt.toml` configuration file that will be automatically applied.

Run `cargo clippy` to catch common mistakes and ensure idiomatic Rust code.

### Testing

When adding new features or fixing bugs, please include appropriate tests. You can run the test suite with:

```bash
cargo test
```

For integration testing with actual image builds, you can use the test configurations in the `tests/ng/` directory.

### Using Just for Development

This project uses [just](https://github.com/casey/just) as a command runner. You can find available commands in the `justfile`:

```bash
# Build the OCI container image
just podman-build

# Run Katsu in a container
just katsu <args>
```

### Pull Request Guidelines

- **Keep PRs focused**: Each PR should address a single concern
- **Write clear descriptions**: Explain what your changes do and why
- **Reference issues**: Link to any related issues using `Fixes #123` or `Relates to #456`
- **Update documentation**: If you're adding features, update relevant documentation
- **Test your changes**: Ensure related tests pass and add new tests as needed
- **Follow commit conventions**: Use conventional commit messages (e.g., `feat:`, `fix:`, `docs:`, `chore:`)

### Reporting Issues

If you find a bug or have a feature request:

1. **Check existing issues** to avoid duplicates
2. **Use issue templates** if available
3. **Provide details**:
   - Katsu version (`katsu --version`)
   - Your operating system and version
   - Steps to reproduce (for bugs)
   - Expected vs. actual behavior
   - Relevant configuration files or error messages

### Getting Help

- Check the [documentation](https://developer.fyralabs.com/katsu)
- Look through existing issues and pull requests
- Join our community discussions (if applicable)

### License

By contributing to Katsu, you agree that your contributions will be licensed under the MIT License.
