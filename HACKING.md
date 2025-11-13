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
