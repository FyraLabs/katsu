use crate::backends::fs_tree::TreeOutput;
use crate::builder::default_true;
use crate::{
	backends::fs_tree::RootBuilder, config::Manifest, feature_flag_bool, feature_flag_str,
};
use bytesize::ByteSize;
use color_eyre::{
	Result,
	eyre::{WrapErr, bail, eyre},
};
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

	/// Embed the OCI image into the chroot's containers-storage. Only used by the
	/// `nested` layout.
	#[serde(default = "default_true")]
	pub embed_image: bool,

	// Nested layout embeds metadata in the tree; OSTree layout writes a workspace sidecar.
	#[serde(default = "default_true")]
	pub embed_image_metadata: bool,

	#[serde(default)]
	pub embed_extra_images: Vec<String>,

	/// Sparse staging disk for the `unified` layout; must hold both the composefs
	/// objects and the imported image store.
	#[serde(default = "default_staging_disk_size")]
	pub staging_disk_size: ByteSize,
}

fn default_staging_disk_size() -> ByteSize {
	// Decimal GB, so it matches the `24G` a user would write.
	ByteSize::gb(24)
}

fn default_stateroot() -> String {
	"um".to_string()
}

/// A mounted image rootfs, unmounted and its container removed on drop.
struct MountedRootfs {
	container: String,
	path: PathBuf,
}

impl MountedRootfs {
	fn path(&self) -> &Path {
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
		// `no-image-pull` builds from an image that is already in local storage, so a
		// build host with a pre-seeded (or air-gapped) store does not need the registry.
		let no_pull = feature_flag_bool!("no-image-pull");
		if no_pull {
			let present = std::process::Command::new("podman")
				.args(["image", "exists", image])
				.status()
				.map(|s| s.success())
				.unwrap_or(false);
			if !present {
				bail!("`no-image-pull` is set but {image} is not in local container storage");
			}
			info!(?image, "Using local base image without pulling");
		} else {
			info!(?image, "Pulling base image");
			cmd_lib::run_cmd!(podman pull $image 2>&1;)?;
		}

		if !no_pull {
			for extra in &self.embed_extra_images {
				info!(?extra, "Pulling extra image to embed");
				cmd_lib::run_cmd!(podman pull $extra 2>&1;)?;
			}
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

	/// Mount the image's rootfs via a throwaway container, removed on drop.
	fn mount_rootfs(image: &str) -> Result<MountedRootfs> {
		let container = cmd_lib::run_fun!(podman create $image /bin/true)?;
		let container = container.trim().to_string();
		debug!(?container, "Created ephemeral container for mounting");

		// Construct the guard first so the container is removed if mounting fails.
		let mut guard = MountedRootfs { container, path: PathBuf::new() };

		let container_name = &guard.container;
		let mountpoint = cmd_lib::run_fun!(podman mount $container_name)?;
		let mountpoint = mountpoint.trim();
		if mountpoint.is_empty() {
			bail!("podman mount returned an empty mountpoint for {}", guard.container);
		}
		guard.path = PathBuf::from(mountpoint);
		info!(?image, path = ?guard.path, "Mounted image rootfs");

		if !guard.path.join("usr").is_dir() {
			bail!("Mounted rootfs at {} has no /usr directory", guard.path.display());
		}

		Ok(guard)
	}

	/// Import the image into `repo` with `ostree container image pull`, returning the
	/// commit checksum.
	fn import_image(repo: &Path, image: &str, digestfile: &Path) -> Result<String> {
		let source = format!("ostree-unverified-image:containers-storage:{image}");
		info!(?repo, ?source, "Importing bootc image with upstream ostree-container importer");
		let status = std::process::Command::new("ostree")
			.args(["container", "image", "pull", "--ostree-digestfile"])
			.arg(digestfile)
			.arg(repo)
			.arg(&source)
			.status()
			.map_err(|e| crate::util::tool_error("ostree", "ostree", e))?;
		if !status.success() {
			bail!(
				"Container import failed ({status}); the build host needs an ostree CLI with `container image pull` support and access to the rootful containers-storage image"
			);
		}
		let checksum = fs::read_to_string(digestfile)?.trim().to_string();
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

	/// Deploy `checksum` from `<sysroot>/ostree/repo`. `ostree admin deploy` fails
	/// early unless `deploy/<stateroot>/var` and `boot/` exist, so create them.
	fn deploy_sysroot(
		sysroot: &Path, stateroot: &str, checksum: &str, origin: &Path,
	) -> Result<()> {
		let stateroot_dir = sysroot.join("ostree/deploy").join(stateroot);
		fs::create_dir_all(sysroot.join("boot"))?;
		fs::create_dir_all(stateroot_dir.join("var"))?;

		info!(?sysroot, ?stateroot, ?checksum, "Deploying imported bootc image into sysroot");
		cmd_lib::run_cmd!(
			ostree admin deploy --sysroot=$sysroot --os=$stateroot --stateroot=$stateroot --origin-file=$origin $checksum 2>&1;
		)?;

		Ok(())
	}

	/// Create a clean sysroot with an empty bare repo. Leftover entries under
	/// `deploy/` make `ostree admin deploy` bail with "Invalid deploy name".
	fn init_sysroot(sysroot: &Path) -> Result<PathBuf> {
		if sysroot.exists() {
			info!(?sysroot, "Clearing existing sysroot before rebuild");
			Self::remove_sysroot(sysroot)?;
		}

		let repo = sysroot.join("ostree/repo");
		fs::create_dir_all(&repo)?;
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

		fs::remove_dir_all(sysroot)
			.wrap_err_with(|| format!("Removing sysroot {}", sysroot.display()))
	}

	/// Derive a default OSTree ref for the commit.
	fn default_branch(&self, rootfs: &Path, manifest: &Manifest) -> Result<String> {
		let arch = manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH);
		let releasever = Self::image_releasever(rootfs)?;
		Ok(format!("{}/{}/{}", self.stateroot, releasever, arch))
	}

	/// Read `VERSION_ID` from the image's os-release.
	fn image_releasever(rootfs: &Path) -> Result<String> {
		let releasever = ["usr/etc/os-release", "etc/os-release", "usr/lib/os-release"]
			.iter()
			.filter_map(|rel| fs::read_to_string(rootfs.join(rel)).ok())
			.find_map(|contents| {
				contents
					.lines()
					.find_map(|line| line.strip_prefix("VERSION_ID="))
					.map(|value| value.trim().trim_matches('"').to_string())
					.filter(|value| !value.is_empty())
			})
			.ok_or_else(|| {
				eyre!(
					"Could not determine the image's release version from os-release; \
					 set `bootc.ref_` explicitly to choose an ostree ref"
				)
			})?;
		debug!(%releasever, "Read release version from image");
		Ok(releasever)
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
		// ostree deploys from the repo in place, so build it at the sysroot to avoid
		// a second ~9G copy.
		let sysroot = workspace.join("bootc-sysroot");
		let repo = Self::init_sysroot(&sysroot)?;

		let branch = match &self.ref_ {
			Some(explicit) => explicit.clone(),
			None => self.default_branch(Self::mount_rootfs(image)?.path(), manifest)?,
		};
		let checksum = Self::import_image(&repo, image, &workspace.join("bootc-imported-commit"))?;
		cmd_lib::run_cmd!(ostree refs --repo=$repo --create=$branch $checksum;)?;
		let origin = workspace.join("bootc-image.origin");
		fs::write(&origin, Self::image_origin(image)?)?;
		if self.embed_image_metadata {
			let metadata =
				BootcImageMetadata { tag: image.to_string(), digest: digest.to_string() };
			fs::write(workspace.join("bootc-image.yaml"), serde_yaml::to_string(&metadata)?)?;
		}

		Self::deploy_sysroot(&sysroot, &self.stateroot, &checksum, &origin)?;

		info!(?sysroot, ?branch, "OSTree sysroot ready for direct boot");
		Ok(TreeOutput::OstreeSysroot { sysroot, stateroot: self.stateroot.clone() })
	}
}

impl BootcRootBuilder {
	/// Build the native composefs unified-storage layout by running bootc's
	/// installer (`to-disk --via-loopback`) against a sparse disk image in the
	/// workspace, then mounting its root partition as the tree.
	fn build_unified(&self, image: &str, workspace: &Path) -> Result<TreeOutput> {
		let sysroot = workspace.join("bootc-sysroot");
		if sysroot.exists() {
			info!(?sysroot, "Clearing existing sysroot before rebuild");
			Self::remove_sysroot(&sysroot)?;
		}
		fs::create_dir_all(&sysroot)?;

		let disk = workspace.join(super::UNIFIED_STAGING_IMAGE);
		if disk.exists() {
			fs::remove_file(&disk)?;
		}
		// Override per-image with `bootc.staging_disk_size`, or per-build with
		// `KATSU_FEATURE_FLAGS=staging-disk-size=32G`.
		let disk_size = feature_flag_str!("staging-disk-size")
			.map(|size| {
				size.parse::<ByteSize>()
					.map_err(|e| eyre!("invalid staging-disk-size {size:?}: {e}"))
			})
			.transpose()?
			.unwrap_or(self.staging_disk_size);
		info!(?disk, bytes = disk_size.as_u64(), "Creating staging disk image");
		// Sparse: only what the install writes is allocated.
		fs::File::create(&disk)?.set_len(disk_size.as_u64())?;

		info!(?disk, ?image, "Installing bootc unified storage onto a staging disk image");
		// Run bootc inside the image: on an ostree-booted host its SELinux probe
		// (`ostree rev-parse --single`) fails with multiple commits. bootc re-execs into
		// the host mount namespace, so pass the disk as an absolute /proc/1/root path.
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
			.status()
			.map_err(|e| crate::util::tool_error("podman", "podman", e))?;
		if !status.success() {
			bail!("bootc unified-storage install failed ({status})");
		}

		let output = TreeOutput::mount_unified(workspace)?;
		info!(?sysroot, "Unified composefs layout ready (bind mount, no copy)");
		Ok(output)
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

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(name);
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		dir
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

	/// Leftover deploy entries not named `<csum>.<treeserial>` make
	/// `ostree admin deploy` bail, so a stale sysroot must be fully removed.
	#[test]
	fn removes_stale_sysroot_contents() {
		let sysroot = scratch("katsu-bootc-stale-sysroot");
		let deploy = sysroot.join("ostree/deploy/um/deploy");
		fs::create_dir_all(deploy.join("iso-tree")).unwrap();
		fs::create_dir_all(deploy.join("0123456789abcdef.0")).unwrap();

		BootcRootBuilder::remove_sysroot(&sysroot).unwrap();

		assert!(!sysroot.exists());
	}

	#[test]
	fn reads_releasever_from_os_release() {
		let cases: &[(&str, &[u8], Option<&str>)] = &[
			("usr/etc/os-release", b"NAME=Ultramarine\nVERSION_ID=43\n", Some("43")),
			("etc/os-release", b"VERSION_ID=\"42\"\n", Some("42")),
			("etc/os-release", b"NAME=Ultramarine\n", None),
		];
		for (i, (rel, contents, expected)) in cases.iter().enumerate() {
			let root = scratch(&format!("katsu-bootc-releasever-{i}"));
			let path = root.join(rel);
			fs::create_dir_all(path.parent().unwrap()).unwrap();
			fs::write(&path, contents).unwrap();

			let got = BootcRootBuilder::image_releasever(&root).ok();
			assert_eq!(got.as_deref(), *expected, "{rel}: {}", String::from_utf8_lossy(contents));
		}
	}
}
