use crate::backends::fs_tree::StagedRoot;
use crate::backends::fs_tree::TreeOutput;
use crate::builder::default_true;
use crate::{backends::fs_tree::RootBuilder, config::Manifest, feature_flag_str};
use bytesize::ByteSize;
use color_eyre::{Result, eyre::bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Metadata for the current image, embedded into derived live images for `bootc install` and debugging
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct BootcImageMetadata {
	/// The original image this was derived from
	pub tag: String,
	/// Image's digest
	pub digest: String,
}

/// How the bootc image should be laid out in the ISO tree.
#[derive(Deserialize, Debug, Clone, Copy, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BootcLayout {
	/// Commit the image's rootfs to an OSTree repo, deploy it into a sysroot, and
	/// boot directly into that deployment. No podman/skopeo is needed at runtime.
	#[default]
	Ostree,
	/// Embed the OCI image into the chroot's own containers-storage so the live
	/// environment can `bootc install` from it.
	Nested,
	/// Native composefs unified storage: the ISO payload is bootc's composefs
	/// layout, whose `composefs/bootc/storage` is a real read-only image store.
	/// Booting uses `composefs=<D>` and installation uses that local store, both
	/// offline. Unlike `Ostree`, no `ostree=` deployment is created.
	Unified,
}

/// A bootc-based image. This is the second implementation of the RootBuilder trait.
/// This takes an OCI image and builds a rootfs out of it, optionally with a containerfile
/// to build a derivation specific to this image.
///
///
/// A derivation is a containerfile with 1 custom argument: `DERIVE_FROM`
///
/// It will be run as `podman build -t <image>:katsu-deriv --build-arg DERIVE_FROM=<image> -f <derivation> <CONTEXT>`
///
/// A containerfile should look like this:
///
/// ```dockerfile
/// ARG DERIVE_FROM
/// FROM $DERIVE_FROM
///
/// RUN echo "Hello from the containerfile!"
/// RUN touch /grass
///
/// # ... Do whatever you want here
/// ```
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct BootcRootBuilder {
	/// The original image to use as a base
	pub image: String,
	/// Path to a containerfile (Dockerfile) to build a derivation out of
	/// (Optional, if not specified, the image will be used as-is)
	pub derivation: Option<String>,
	pub context: Option<String>,

	/// How to lay the image out on the resulting media
	#[serde(default)]
	pub layout: BootcLayout,

	/// Additional OSTree ref for the imported image, e.g. `um/44/x86_64`.
	/// Defaults to a value derived from the image's distro metadata.
	#[serde(default)]
	pub ref_: Option<String>,

	/// OSTree stateroot (a.k.a. `--os` in ostree terms)
	#[serde(default = "default_stateroot")]
	pub stateroot: String,

	/// Legacy nested-store layout: embed the OCI image into the chroot's
	/// containers-storage. Ignored when `layout` is `ostree`.
	#[serde(default = "default_true")]
	pub embed_image: bool,

	// Nested layout embeds metadata in the tree; OSTree layout writes a workspace sidecar.
	#[serde(default = "default_true")]
	pub embed_image_metadata: bool,

	#[serde(default)]
	pub embed_extra_images: Vec<String>,

	/// Size of the staging disk image used by the `unified` layout.
	///
	/// The unified install writes the composefs objects and the imported image
	/// store separately before dedup, so the staging filesystem must hold both.
	/// Size this from the image rather than assuming a constant; the file is
	/// sparse, but it must fit once populated.
	#[serde(default = "default_staging_disk_size")]
	pub staging_disk_size: ByteSize,
}

fn default_staging_disk_size() -> ByteSize {
	// Use decimal GB so the default matches what a user writes with the `24G`
	// shorthand; `ByteSize::gib` would silently be ~7% larger than `24G`.
	ByteSize::gb(24)
}

fn default_stateroot() -> String {
	"um".to_string()
}

/// A mounted image rootfs, unmounted and its container removed on drop.
///
/// Holds the mount for as long as the tree is needed and cleans up on every
/// exit path, including early returns from `?`.
pub struct MountedRootfs {
	container: String,
	path: PathBuf,
}

impl MountedRootfs {
	fn with_path(mut self, path: PathBuf) -> Self {
		self.path = path;
		self
	}

	pub fn path(&self) -> &Path {
		&self.path
	}
}

impl Drop for MountedRootfs {
	fn drop(&mut self) {
		let container = self.container.clone();
		if let Err(err) = cmd_lib::run_cmd!(podman unmount $container 2>/dev/null;) {
			debug!(?err, %container, "Unmounting rootfs failed");
		}
		if let Err(err) = cmd_lib::run_cmd!(podman rm -f $container 2>/dev/null;) {
			debug!(?err, %container, "Removing rootfs container failed");
		}
	}
}

impl BootcRootBuilder {
	/// Embeds an OCI image into the container store (legacy nested layout)
	fn embed_image_to_store(
		image: &str, container_store: &Path, container: &str,
	) -> Result<PathBuf> {
		let container_store_display = container_store.display();
		info!(?image, "Copying OCI image to chroot's container store");

		// Create a temporary storage.conf in /run that uses fuse-overlayfs for nested overlay support
		let storage_conf_path = Path::new("/run").join("katsu-storage.conf");
		let storage_conf = r#"[storage]
driver = "overlay"

[storage.options]
mount_program = "/usr/bin/fuse-overlayfs"

[storage.options.overlay]
mount_program = "/usr/bin/fuse-overlayfs"
"#
		.to_string();
		std::fs::write(&storage_conf_path, storage_conf)?;

		// Use skopeo to copy the image from containers-storage to the destination
		// skopeo handles copying layers properly between different storage configurations
		let dest_image = image.split('@').next().unwrap_or(image);
		let storage_conf_env = storage_conf_path.display();
		cmd_lib::run_cmd!(
			CONTAINERS_STORAGE_CONF=${storage_conf_env} skopeo copy --dest-compress --remove-signatures "containers-storage:${image}" "containers-storage:[${container_store_display}]${dest_image}";
		)?;

		// quirk: After we push the image, podman will unmount the entire container store, so we have to remount it
		let new_mountpoint = cmd_lib::run_fun!(
			podman mount $container
		)?;
		Ok(Path::new(new_mountpoint.trim()).to_path_buf())
	}

	/// Pull the base image, building a derivation first if one is configured.
	/// Returns the image reference to use for the rest of the build.
	fn resolve_image(&self, image: &str) -> Result<String> {
		info!(?image, "Pulling base image");
		cmd_lib::run_cmd!(podman pull $image 2>&1;)?;

		for extra in &self.embed_extra_images {
			info!(?extra, "Pulling extra image to embed");
			cmd_lib::run_cmd!(podman pull $extra 2>&1;)?;
		}

		let Some(derivation) = &self.derivation else {
			return Ok(image.to_string());
		};

		let context = self.context.as_deref().unwrap_or(".");
		let og_image = image.split(':').next().unwrap_or(image);
		let deriv = format!("{og_image}:katsu_deriv");

		info!(?deriv, ?derivation, "Building image derivation");
		cmd_lib::run_cmd!(
			podman build -t $deriv --network host --build-arg DERIVE_FROM=$image -f $derivation $context;
		)?;

		Ok(deriv)
	}

	/// Image digest, used for metadata and cache naming.
	fn image_digest(image: &str) -> Result<String> {
		let digest = cmd_lib::run_fun!(
			podman inspect --format="{{index .Digest}}" $image
		)?;
		Ok(digest.trim().to_string())
	}

	/// Materialize the image's rootfs as a writable directory tree.
	///
	/// Mounts the merged view directly; exporting through a tar stream would copy
	/// the whole filesystem (several GB) only to unpack it again. The mount is
	/// writable because the tree is mutated in place to prepare `/etc` for commit.
	fn mount_rootfs(image: &str) -> Result<MountedRootfs> {
		let container = cmd_lib::run_fun!(podman create $image /bin/true)?;
		let container = container.trim().to_string();
		debug!(?container, "Created ephemeral container for mounting");

		let guard = MountedRootfs { container, path: PathBuf::new() };

		let container_name = guard.container.clone();
		let mountpoint = cmd_lib::run_fun!(podman mount $container_name)?;
		let mountpoint = mountpoint.trim().to_string();
		if mountpoint.is_empty() {
			bail!("podman mount returned an empty mountpoint for {}", guard.container);
		}
		let path = PathBuf::from(mountpoint);
		info!(?image, ?path, "Mounted image rootfs");

		// A rootfs without /usr is not a usable image tree; catch that here rather
		// than failing obscurely during the commit.
		if !path.join("usr").is_dir() {
			bail!("Mounted rootfs at {} has no /usr directory", path.display());
		}

		Ok(guard.with_path(path))
	}

	/// Prepare a rootfs for `ostree commit`.
	///
	/// OSTree wants vendor config in `/usr/etc` and the machine-local merge point
	/// in `/etc`; bootc images ship everything in `/etc` and have no `/usr/etc`,
	/// so relocate and leave an empty placeholder. Both populated is rejected by
	/// `ostree admin deploy`.
	#[cfg(test)]
	fn prepare_etc_for_commit(rootfs: &Path) -> Result<()> {
		let etc = rootfs.join("etc");
		let usr_etc = rootfs.join("usr/etc");

		if usr_etc.exists() {
			// Already an ostree-shaped tree (e.g. a pre-composed rootfs).
			debug!(?usr_etc, "Rootfs already has /usr/etc, leaving as-is");
			if etc.exists() {
				bail!(
					"Tree contains both /etc and /usr/etc; refusing to commit an ambiguous layout"
				);
			}
			return Ok(());
		}

		if !etc.is_dir() {
			bail!("Image has no /etc directory, cannot prepare for ostree commit");
		}

		info!("Relocating /etc to /usr/etc for ostree's vendor/machine config split");
		std::fs::create_dir_all(&usr_etc)?;

		// Move entry-by-entry so we do not have to create a then-delete dir tree.

		for entry in std::fs::read_dir(&etc)? {
			let entry = entry?;
			let src = entry.path();
			let dest = usr_etc.join(entry.file_name());
			Self::move_path(&src, &dest)?;
		}

		// `ostree admin deploy` requires an (empty) /etc in the commit.
		std::fs::create_dir_all(&etc)?;

		Ok(())
	}

	/// Move `src` to `dest`, falling back to a recursive copy when a rename is not
	/// possible (for example across overlayfs layers).
	#[cfg(test)]
	fn move_path(src: &Path, dest: &Path) -> Result<()> {
		match std::fs::rename(src, dest) {
			Ok(()) => Ok(()),
			Err(err) => {
				// EXDEV (cross-device) is expected on overlayfs; anything else we
				// still attempt the copy, which gives a more accurate error if it
				// fails for a real reason (e.g. permissions).
				debug!(?err, ?src, ?dest, "rename failed, falling back to copy");
				Self::copy_path(src, dest)?;
				Self::remove_path(src)
			},
		}
	}

	/// Recursively copy a file, directory or symlink, preserving symlinks.
	#[cfg(test)]
	fn copy_path(src: &Path, dest: &Path) -> Result<()> {
		let meta = std::fs::symlink_metadata(src)?;

		if meta.file_type().is_symlink() {
			use std::os::unix::fs::symlink;
			let target = std::fs::read_link(src)?;
			let _ = std::fs::remove_file(dest);
			symlink(target, dest)?;
			return Ok(());
		}

		if meta.is_dir() {
			std::fs::create_dir_all(dest)?;
			// Preserve the directory's own permissions; content is copied below.
			let _ = std::fs::set_permissions(dest, meta.permissions());
			for entry in std::fs::read_dir(src)? {
				let entry = entry?;
				Self::copy_path(&entry.path(), &dest.join(entry.file_name()))?;
			}
			return Ok(());
		}

		std::fs::copy(src, dest)?;
		std::fs::set_permissions(dest, meta.permissions())?;
		Ok(())
	}

	/// Remove a file, symlink or directory tree.
	#[cfg(test)]
	fn remove_path(path: &Path) -> Result<()> {
		let meta = std::fs::symlink_metadata(path)?;
		if meta.is_dir() {
			std::fs::remove_dir_all(path)?;
		} else {
			std::fs::remove_file(path)?;
		}
		Ok(())
	}

	/// Remove paths that must not be part of the committed tree.
	///
	/// These are runtime state or self-referential plumbing from the image build:
	/// the `sysroot/ostree/ostree -> /sysroot/ostree` symlink loop would make
	/// ostree recurse into itself, and `/var` state is not deployment content.
	#[cfg(test)]
	fn sanitize_rootfs_for_commit(rootfs: &Path) -> Result<()> {
		// Self-referential symlink left behind by bootc's image layout.
		let sysroot = rootfs.join("sysroot");
		if sysroot.exists() {
			debug!(?sysroot, "Removing /sysroot from committed tree");
			let _ = std::fs::remove_dir_all(&sysroot);
		}

		for volatile in ["tmp", "run"] {
			let path = rootfs.join(volatile);
			if path.exists() {
				debug!(?path, "Clearing volatile directory in committed tree");
				let _ = std::fs::remove_dir_all(&path);
				std::fs::create_dir_all(&path)?;
			}
		}

		Ok(())
	}

	/// Import real OCI layer state and metadata, rather than committing a flattened tree.
	fn import_image(repo: &Path, image: &str, digestfile: &Path) -> Result<String> {
		let source = format!("ostree-unverified-image:containers-storage:{image}");
		info!(?repo, ?source, "Importing bootc image with upstream ostree-container importer");
		let status = std::process::Command::new("ostree")
			.args(["container", "image", "pull", "--ostree-digestfile"])
			.arg(digestfile)
			.arg(repo)
			.arg(&source)
			.status()?;
		if !status.success() {
			bail!(
				"Container import failed ({status}); the build host needs an ostree CLI with `container image pull` support and access to the rootful containers-storage image"
			);
		}
		let checksum = std::fs::read_to_string(digestfile)?.trim().to_string();
		if checksum.len() != 64 || !checksum.bytes().all(|b| b.is_ascii_hexdigit()) {
			bail!("Container importer returned invalid OSTree checksum: {checksum}");
		}
		Ok(checksum)
	}

	fn image_origin(image: &str) -> Result<String> {
		if image.is_empty() || image.chars().any(char::is_control) {
			bail!("Invalid container image reference for deployment origin");
		}
		// The source is already trusted locally; do not claim signature verification.
		Ok(format!("[origin]\ncontainer-image-reference=ostree-unverified-registry:{image}\n"))
	}

	/// Deploy a committed branch into a sysroot so it can be booted directly.
	///
	/// `repo` must already live at `<sysroot>/ostree/repo`; ostree deploys
	/// directly from it, so no repo copy is needed. Committing in place avoids
	/// carrying two full copies of the repository (~9G for a bootc image) through
	/// the build, which matters because the squash step needs headroom too.
	///
	/// `ostree admin deploy` expects a fairly specific shape and will fail early
	/// on each missing piece, so we pre-create the directories it looks for:
	/// `deploy/<stateroot>/var` and `boot/`.
	fn deploy_sysroot(
		sysroot: &Path, stateroot: &str, checksum: &str, origin: &Path,
	) -> Result<()> {
		let stateroot_dir = sysroot.join("ostree/deploy").join(stateroot);
		std::fs::create_dir_all(&stateroot_dir)?;
		std::fs::create_dir_all(sysroot.join("boot"))?;
		std::fs::create_dir_all(stateroot_dir.join("var"))?;

		info!(?sysroot, ?stateroot, ?checksum, "Deploying imported bootc image into sysroot");
		cmd_lib::run_cmd!(
			ostree admin deploy --sysroot=$sysroot --os=$stateroot --stateroot=$stateroot --origin-file=$origin $checksum 2>&1;
		)?;

		Ok(())
	}

	/// Create a fresh sysroot with an empty OSTree repository at
	/// `<sysroot>/ostree/repo`, ready to be committed into.
	///
	/// `ostree admin deploy` performs an initial cleanup pass that validates every
	/// entry under `ostree/deploy/<stateroot>/deploy` against
	/// `<checksum>.<treeserial>`, so leftover content from an earlier run (for
	/// example a stale directory) makes it bail with "Invalid deploy name". We
	/// therefore always start from a clean sysroot.
	fn init_sysroot(sysroot: &Path) -> Result<PathBuf> {
		if sysroot.exists() {
			info!(?sysroot, "Clearing existing sysroot before rebuild");
			Self::remove_sysroot(sysroot)?;
		}

		let repo = sysroot.join("ostree/repo");
		std::fs::create_dir_all(&repo)?;
		cmd_lib::run_cmd!(ostree init --repo=$repo --mode=bare 2>&1;)?;
		Ok(repo)
	}

	/// Recursively remove a sysroot, clearing ostree's immutable bit first.
	///
	/// `ostree admin deploy` sets `chattr +i` on deployment roots to prevent
	/// mutation, which makes them undeletable until the flag is cleared.
	fn remove_sysroot(sysroot: &Path) -> Result<()> {
		// Best-effort: a tree without immutable bits will report an error here,
		// which we do not care about.
		let _ = cmd_lib::run_cmd!(chattr -R -i $sysroot 2>/dev/null;);

		std::fs::remove_dir_all(sysroot)
			.map_err(|e| color_eyre::eyre::eyre!("Removing sysroot {}: {e}", sysroot.display()))?;
		Ok(())
	}

	/// Derive a default OSTree ref for the commit.
	fn default_branch(&self, rootfs: &Path, manifest: &Manifest) -> Result<String> {
		let arch = manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH);
		let releasever = Self::image_releasever(rootfs)?;
		Ok(format!("{}/{}/{}", self.stateroot, releasever, arch))
	}

	/// Read `VERSION_ID` from the image's `/usr/etc/os-release` (or `/etc/os-release`).
	fn image_releasever(rootfs: &Path) -> Result<String> {
		for rel in ["usr/etc/os-release", "etc/os-release", "usr/lib/os-release"] {
			let path = rootfs.join(rel);
			let Ok(contents) = std::fs::read_to_string(&path) else {
				continue;
			};
			for line in contents.lines() {
				if let Some(value) = line.strip_prefix("VERSION_ID=") {
					let value = value.trim().trim_matches('"');
					if !value.is_empty() {
						debug!(?path, releasever = value, "Read release version from image");
						return Ok(value.to_string());
					}
				}
			}
		}

		bail!(
			"Could not determine the image's release version from os-release; \
			 set `bootc.ref_` explicitly to choose an ostree ref"
		)
	}

	/// Build the image into an OSTree sysroot that can be booted directly.
	///
	/// Returns the sysroot path. The ISO builder treats this as the tree root and
	/// erofs's it, which means `/ostree/repo` (and therefore the deployment) ships
	/// on the media. Booting then uses the `ostree=` kernel argument generated in
	/// the BLS entry, with no podman/skopeo/overlayfs needed at runtime.
	fn build_ostree_sysroot(
		&self, image: &str, digest: &str, workspace: &Path, manifest: &Manifest,
	) -> Result<TreeOutput> {
		let mounted = Self::mount_rootfs(image)?;
		let rootfs = mounted.path();

		// Commit straight into a repository living at the sysroot path. ostree
		// deploys from a repo in place, so building it anywhere else would mean
		// copying it (a second full copy of ~9G for a bootc image) before the
		// squash step, which itself needs headroom.
		let sysroot = workspace.join("bootc-sysroot");
		let repo = Self::init_sysroot(&sysroot)?;

		let branch = match &self.ref_ {
			Some(explicit) => {
				debug!(?explicit, "Using explicitly configured ostree ref");
				explicit.clone()
			},
			None => self.default_branch(rootfs, manifest)?,
		};
		// Only inspect the mount for distro metadata. Import the original image
		// unchanged so its manifest and layer identities remain meaningful.
		drop(mounted);
		let checksum = Self::import_image(&repo, image, &workspace.join("bootc-imported-commit"))?;
		cmd_lib::run_cmd!(ostree refs --repo=$repo --create=$branch $checksum;)?;
		let origin = workspace.join("bootc-image.origin");
		std::fs::write(&origin, Self::image_origin(image)?)?;
		if self.embed_image_metadata {
			let metadata =
				BootcImageMetadata { tag: image.to_string(), digest: digest.to_string() };
			std::fs::write(workspace.join("bootc-image.yaml"), serde_yaml::to_string(&metadata)?)?;
		}

		Self::deploy_sysroot(&sysroot, &self.stateroot, &checksum, &origin)?;

		info!(?sysroot, ?branch, "OSTree sysroot ready for direct boot");
		Ok(TreeOutput::OstreeSysroot { sysroot, stateroot: self.stateroot.clone() })
	}
}

impl BootcRootBuilder {
	/// Build the native composefs unified-storage layout.
	///
	/// bootc's installer is the only supported producer of this layout: it creates
	/// the composefs repository, imports the image zero-copy into a bootc-owned
	/// containers-storage on the **target** filesystem, and generates the BLS
	/// entry selecting `composefs=<D>`.
	///
	/// Install runs against a fresh sparse disk image, then the populated root
	/// filesystem is extracted for the ISO. That avoids needing a real block device
	/// and keeps the composefs object/store sharing intact (both live in one
	/// filesystem).
	/// Build the native composefs unified-storage layout.
	///
	/// bootc's installer is the only supported producer of this layout: it creates
	/// the composefs repository, imports the image zero-copy into a bootc-owned
	/// containers-storage on the **target** filesystem, and generates the BLS entry
	/// selecting `composefs=<D>`.
	///
	/// Install runs against a fresh sparse disk image via `to-disk --via-loopback`,
	/// then the populated root filesystem is copied out for the ISO. The sparse file
	/// lives in the workspace so its size is bounded and it is cleaned up with the
	/// rest of the build.
	fn build_unified(&self, image: &str, workspace: &Path) -> Result<TreeOutput> {
		let sysroot = workspace.join("bootc-sysroot");
		if sysroot.exists() {
			info!(?sysroot, "Clearing existing sysroot before rebuild");
			Self::remove_sysroot(&sysroot)?;
		}
		std::fs::create_dir_all(&sysroot)?;

		// bootc's own tooling expects to find the image in a containers-storage it
		// can read; the build host already has it in the rootful podman store.
		let disk = workspace.join("unified.raw");
		if disk.exists() {
			std::fs::remove_file(&disk)?;
		}
		// A freshly formatted btrfs root needs room for both the composefs objects
		// and the imported image store, which share extents but are written
		// separately before dedup.
		// The staging filesystem must hold the composefs objects and the imported
		// image store, which are written separately before dedup. Defaults to 24G;
		// override per-image with `bootc.staging_disk_size` or for a one-off build
		// with `KATSU_FEATURE_FLAGS=staging-disk-size=32G`.
		let disk_size = feature_flag_str!("staging-disk-size")
			.map(|size| {
				size.parse::<ByteSize>()
					.map_err(|e| color_eyre::eyre::eyre!("invalid staging-disk-size {size:?}: {e}"))
			})
			.transpose()?
			.unwrap_or(self.staging_disk_size);
		// Use the raw byte count: `ByteSize`'s Display renders "24.0 GB", which
		// `fallocate` rejects as an invalid length.
		let disk_bytes = disk_size.as_u64();
		info!(?disk, bytes = disk_bytes, "Creating staging disk image");
		// A sparse file: `set_len` extends the length without allocating blocks, so
		// the image only occupies what the install actually writes.
		fs::File::create(&disk)?.set_len(disk_bytes)?;

		info!(?disk, ?image, "Installing bootc unified storage onto a staging disk image");
		// bootc must not run directly on the build host: `SourceInfo` shells out to
		// `ostree --repo=/ostree/repo rev-parse --single` to detect SELinux labels,
		// which fails with "Multiple commit objects found" on an ostree-booted host
		// that has more than one commit. Running inside the source image with
		// --pid=host avoids that path entirely, and bootc then detects its own image
		// without needing `--source-imgref`.
		//
		// The disk is passed as a real host path (`/proc/1/root/...`) because bootc
		// re-execs into the host mount namespace, where a container bind mount would
		// no longer exist. The path must be absolute: `/proc/1/root` is a prefix, so
		// a relative path would concatenate into `/proc/1/rootkatsu-work/...`.
		let abs_disk = disk.canonicalize()?;
		let host_disk = format!("/proc/1/root{}", abs_disk.display());
		let status = std::process::Command::new("podman")
			.args(["run", "--rm", "--privileged", "--pid=host"])
			.args(["--security-opt", "label=type:unconfined_t"])
			.args(["-v", "/dev:/dev"])
			.args(["--memory=8g"])
			.arg(image)
			.args(["bootc", "install", "to-disk", "--via-loopback"])
			.args(["--composefs-backend", "--experimental-unified-storage"])
			.args(["--filesystem=btrfs", "--generic-image", "--skip-fetch-check"])
			.arg(format!("--target-imgref={image}"))
			.arg(&host_disk)
			.status()?;
		if !status.success() {
			bail!("bootc unified-storage install failed ({status})");
		}

		// Copy the written root filesystem into place. The installer mounts and
		// unmounts the target itself, so re-attach the image's root partition just
		// long enough to expose the tree, then release everything on drop.
		// The image is partitioned, so the root partition must be visible to mount.
		// `loopdev_with_file` attaches without scanning, and only the bare device node
		// appears; use the builder to force a partition scan.
		let (loop_dev, loop_hdl) = crate::util::loopdev_with_file_and_parts(&disk)?;
		let loop_dev = loop_dev.to_string_lossy().to_string();
		// `to-disk` finishes by unmounting what it wrote, but the loop device and its
		// partitions can need a moment before they are usable again.
		cmd_lib::run_cmd!(udevadm settle;)?;
		let part = format!("{loop_dev}p3");

		let staging = workspace.join("unified-root");
		if staging.exists() {
			std::fs::remove_dir_all(&staging)?;
		}
		std::fs::create_dir_all(&staging)?;
		cmd_lib::run_cmd!(mount -o ro $part $staging;)?;

		// Verify the layout is what we expect before handing it to the ISO phases,
		// so a partial install fails here rather than during squash.
		if !staging.join("composefs").is_dir() {
			bail!("Unified install produced no composefs in {}", staging.display());
		}

		// Expose the installed tree through an overlayfs rather than copying it: the
		// staging root is the read-only lower layer and an upper absorbs any
		// build-time mutation, so nothing is written back to the image and no ~13G
		// copy is needed. This matches how the system will run live, and the upper
		// layer is discarded with the mount.
		let upper = workspace.join("unified-overlay");
		for dir in ["upper", "work"] {
			let path = upper.join(dir);
			if path.exists() {
				std::fs::remove_dir_all(&path)?;
			}
			std::fs::create_dir_all(&path)?;
		}
		std::fs::create_dir_all(&sysroot)?;
		let lower = staging.display();
		let upp = upper.join("upper");
		let work = upper.join("work");
		let target = sysroot.display();
		let overlay = sysroot.clone();
		cmd_lib::run_cmd!(
			mount -t overlay overlay -o lowerdir=$lower,upperdir=$upp,workdir=$work $target;
		)?;

		// The mounts and loop device must outlive this function, so they travel with
		// the returned output and are released when the build drops it.
		info!(?sysroot, "Unified composefs layout ready (overlay, no copy)");
		Ok(TreeOutput::UnifiedSysroot {
			sysroot,
			staging_image: workspace.join(crate::backends::fs_tree::UNIFIED_STAGING_IMAGE),
			_loop: loop_hdl,
			_mounts: vec![StagedRoot::new(staging), StagedRoot::new(overlay)],
		})
	}
}

impl RootBuilder for BootcRootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<TreeOutput> {
		let image = &self.image;
		let d_image = self.resolve_image(image)?;
		let digest = Self::image_digest(&d_image)?;
		let workspace = chroot.parent().unwrap_or(chroot);

		match self.layout {
			BootcLayout::Ostree => {
				self.build_ostree_sysroot(&d_image, &digest, workspace, manifest)
			},
			BootcLayout::Nested => self.build_nested(chroot, image, &d_image, &digest),
			BootcLayout::Unified => self.build_unified(&d_image, workspace),
		}
	}
}

impl BootcRootBuilder {
	/// Legacy layout: mount the container rootfs and nest the OCI image inside it.
	fn build_nested(
		&self, chroot: &Path, image: &str, d_image: &str, digest: &str,
	) -> Result<TreeOutput> {
		info!("Building nested container-store layout");
		std::fs::create_dir_all(chroot)?;

		let digest_trimmed = digest.trim_start_matches("sha256:").get(0..7).unwrap_or("unknown");
		let container_name = format!("katsu-{digest_trimmed}");

		let existing_container = cmd_lib::run_fun!(
			podman ps -a --filter name=^${container_name} --format="{{.ID}}"
		)
		.unwrap_or_default();

		let container = if !existing_container.trim().is_empty() {
			info!(?container_name, "Reusing existing container");
			existing_container.trim().to_string()
		} else {
			info!(?container_name, "Creating new container");
			cmd_lib::run_fun!(
				podman create --rm --name ${container_name} $d_image /bin/bash
			)?
			.trim()
			.to_string()
		};

		// experiment: mount container's root fs directly
		let mountpoint = cmd_lib::run_fun!(podman mount $container)?;
		let mut mountpoint = Path::new(mountpoint.trim()).to_path_buf();
		info!(?mountpoint, "Mountpoint for container's rootfs");

		let container_store = mountpoint.canonicalize()?.join("var/lib/containers/storage");
		std::fs::create_dir_all(&container_store)?;

		let mut images_to_embed: Vec<String> = self.embed_extra_images.clone();
		if self.embed_image {
			images_to_embed.insert(0, image.to_string());
		}

		for image_to_embed in &images_to_embed {
			mountpoint = Self::embed_image_to_store(image_to_embed, &container_store, &container)?;
		}

		if self.embed_image_metadata {
			let metadata =
				BootcImageMetadata { tag: image.to_string(), digest: digest.to_string() };
			let serialized = serde_yaml::to_string(&metadata)?;
			info!(?serialized, "Embedding image metadata into derived image");
			std::fs::write(mountpoint.join(".bootc_meta.yaml"), serialized)?;
		}

		Ok(TreeOutput::Directory(mountpoint))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(name);
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		dir
	}

	/// A bootc/OCI rootfs keeps everything in /etc and has no /usr/etc.
	#[test]
	fn moves_etc_into_usr_etc_and_leaves_empty_placeholder() {
		let root = scratch("katsu-bootc-etc");
		fs::create_dir_all(root.join("etc/ssh")).unwrap();
		fs::write(root.join("etc/ssh/sshd_config"), b"x").unwrap();
		fs::write(root.join("etc/os-release"), b"x").unwrap();

		BootcRootBuilder::prepare_etc_for_commit(&root).unwrap();

		assert!(root.join("usr/etc/ssh/sshd_config").is_file());
		assert!(root.join("usr/etc/os-release").is_file());
		// /etc must remain, but be empty so deploy does not complain about both.
		assert!(root.join("etc").is_dir());
		assert_eq!(fs::read_dir(root.join("etc")).unwrap().count(), 0);
	}

	/// An already-composed tree must be left untouched.
	#[test]
	fn staging_disk_size_is_configurable_and_defaults_to_24gib() {
		use bytesize::ByteSize;

		let default: BootcRootBuilder = serde_yaml::from_str("image: example.com/os:1").unwrap();
		assert_eq!(default.staging_disk_size, ByteSize::gb(24));

		// A typo would silently keep the default, so assert an explicit value lands.
		// Note `48G` is decimal gigabytes, matching the default's scale rather than
		// the binary `ByteSize::gib` form.
		let configured: BootcRootBuilder =
			serde_yaml::from_str("image: example.com/os:1\nstaging_disk_size: 48G").unwrap();
		assert_eq!(configured.staging_disk_size, ByteSize::gb(48));
	}

	#[test]
	fn image_origin_uses_container_reference_without_plain_refspec() {
		let origin =
			BootcRootBuilder::image_origin("ghcr.io/ultramarine-linux/plasma-bootc:44").unwrap();
		assert_eq!(
			origin,
			"[origin]\ncontainer-image-reference=ostree-unverified-registry:ghcr.io/ultramarine-linux/plasma-bootc:44\n"
		);
		assert!(!origin.contains("refspec="));
		assert!(BootcRootBuilder::image_origin("image\nrefspec=other").is_err());
		assert!(BootcRootBuilder::image_origin("").is_err());
	}

	#[test]
	fn leaves_usr_etc_tree_alone() {
		let root = scratch("katsu-bootc-usr-etc");
		fs::create_dir_all(root.join("usr/etc")).unwrap();
		fs::write(root.join("usr/etc/os-release"), b"x").unwrap();

		BootcRootBuilder::prepare_etc_for_commit(&root).unwrap();

		assert!(root.join("usr/etc/os-release").is_file());
	}

	/// Both populated is ambiguous and must be refused.
	#[test]
	fn rejects_tree_with_both_etc_and_usr_etc() {
		let root = scratch("katsu-bootc-both-etc");
		fs::create_dir_all(root.join("etc")).unwrap();
		fs::create_dir_all(root.join("usr/etc")).unwrap();
		fs::write(root.join("etc/os-release"), b"x").unwrap();
		fs::write(root.join("usr/etc/os-release"), b"x").unwrap();

		assert!(BootcRootBuilder::prepare_etc_for_commit(&root).is_err());
	}

	/// The self-referential /sysroot/ostree/ostree symlink would make ostree
	/// recurse into itself during commit.
	#[test]
	fn removes_self_referential_sysroot() {
		let root = scratch("katsu-bootc-sysroot");
		fs::create_dir_all(root.join("ostree")).unwrap();
		fs::create_dir_all(root.join("sysroot/ostree")).unwrap();
		std::os::unix::fs::symlink("/sysroot/ostree", root.join("sysroot/ostree/ostree")).unwrap();

		BootcRootBuilder::sanitize_rootfs_for_commit(&root).unwrap();

		assert!(!root.join("sysroot").exists());
		assert!(root.join("ostree").exists());
	}

	/// A leftover sysroot makes `ostree admin deploy` bail during its initial
	/// cleanup, because it rejects any deploy entry not named `<csum>.<treeserial>`.
	#[test]
	fn removes_stale_sysroot_contents() {
		let sysroot = scratch("katsu-bootc-stale-sysroot");
		// Content from a previous run, including a directory that is not a valid
		// deployment name.
		let deploy = sysroot.join("ostree/deploy/um/deploy");
		fs::create_dir_all(deploy.join("iso-tree")).unwrap();
		fs::create_dir_all(deploy.join("0123456789abcdef.0")).unwrap();

		BootcRootBuilder::remove_sysroot(&sysroot).unwrap();

		assert!(!sysroot.exists());
	}

	#[test]
	fn relocates_etc_preserving_structure_and_symlinks() {
		let root = scratch("katsu-bootc-etc-relocate");
		let etc = root.join("etc");

		// Nested directory with a file.
		fs::create_dir_all(etc.join("ssh")).unwrap();
		fs::write(etc.join("ssh/sshd_config"), b"x").unwrap();
		// A plain file at the top level.
		fs::write(etc.join("os-release"), b"x").unwrap();
		// A symlink, as images commonly have for e.g. /etc/localtime.
		std::os::unix::fs::symlink("/usr/share/zoneinfo/UTC", etc.join("localtime")).unwrap();

		BootcRootBuilder::prepare_etc_for_commit(&root).unwrap();

		assert!(root.join("usr/etc/ssh/sshd_config").is_file());
		assert!(root.join("usr/etc/os-release").is_file());

		let localtime = root.join("usr/etc/localtime");
		assert!(localtime.is_symlink(), "symlink should stay a symlink");
		assert_eq!(fs::read_link(&localtime).unwrap().to_string_lossy(), "/usr/share/zoneinfo/UTC");

		// /etc must remain, but be empty.
		assert_eq!(fs::read_dir(root.join("etc")).unwrap().count(), 0);
	}

	/// The `move_path` fallback must produce the same result as a rename.
	#[test]
	fn move_path_moves_directories_recursively() {
		let root = scratch("katsu-bootc-move-path");
		let src = root.join("src/nested");
		fs::create_dir_all(&src).unwrap();
		fs::write(src.join("file"), b"content").unwrap();

		let dest = root.join("dest");
		BootcRootBuilder::move_path(&root.join("src"), &dest).unwrap();

		assert!(dest.join("nested/file").is_file());
		assert_eq!(fs::read(dest.join("nested/file")).unwrap(), b"content");
		assert!(!root.join("src").exists(), "source should be removed after a move");
	}

	#[test]
	fn reads_releasever_from_usr_etc_os_release() {
		let root = scratch("katsu-bootc-releasever");
		fs::create_dir_all(root.join("usr/etc")).unwrap();
		fs::write(root.join("usr/etc/os-release"), b"NAME=Ultramarine\nVERSION_ID=43\n").unwrap();

		assert_eq!(BootcRootBuilder::image_releasever(&root).unwrap(), "43");
	}

	#[test]
	fn strips_quotes_from_releasever() {
		let root = scratch("katsu-bootc-releasever-quoted");
		fs::create_dir_all(root.join("etc")).unwrap();
		fs::write(root.join("etc/os-release"), b"VERSION_ID=\"42\"\n").unwrap();

		assert_eq!(BootcRootBuilder::image_releasever(&root).unwrap(), "42");
	}

	/// We must not silently invent a version when the image does not declare one.
	#[test]
	fn errors_when_image_declares_no_version() {
		let root = scratch("katsu-bootc-no-releasever");
		fs::create_dir_all(root.join("etc")).unwrap();
		fs::write(root.join("etc/os-release"), b"NAME=Ultramarine\n").unwrap();

		assert!(BootcRootBuilder::image_releasever(&root).is_err());
	}
}
