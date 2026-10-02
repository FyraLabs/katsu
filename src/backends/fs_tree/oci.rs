use crate::backends::fs_tree::TreeOutput;
use crate::builder::default_true;
use crate::{backends::fs_tree::RootBuilder, config::Manifest};
use color_eyre::{Result, eyre::bail};
use serde::{Deserialize, Serialize};
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

	/// OSTree ref to commit the image as, e.g. `um/44/x86_64`.
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

	// Embed image metadata on derived images
	#[serde(default = "default_true")]
	pub embed_image_metadata: bool,

	#[serde(default)]
	pub embed_extra_images: Vec<String>,
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
	/// We mount the image's merged view directly rather than streaming it through
	/// `podman export | tar -x`. Export serializes the whole filesystem to a tar
	/// stream (several GB for a bootc image) purely to deserialize it straight
	/// back onto disk, whereas the mount gives us the tree as-is.
	///
	/// The returned guard must be kept alive for as long as the tree is needed,
	/// and dropped to unmount. The mount is writable, which we require: the tree
	/// is mutated in place to prepare `/etc` for `ostree commit`. Because these
	/// writes dirty the container's overlay upper layer, the container is
	/// ephemeral and never reused.
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
	/// OSTree deployments want the vendor configuration split from machine-local
	/// configuration: `/usr/etc` is the immutable vendor default and `/etc` is the
	/// mutable, 3-way-merged location generated at deploy time.
	///
	/// bootc/OCI images ship everything directly in `/etc` and have no `/usr/etc`
	/// at all, so we relocate `/etc` into `/usr/etc` and leave an *empty* `/etc`
	/// placeholder. A tree containing both a populated `/etc` and `/usr/etc` is
	/// rejected by `ostree admin deploy` with
	/// "Tree contains both /etc and /usr/etc", so the placeholder must stay empty.
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
			let dest = usr_etc.join(entry.file_name());
			std::fs::rename(entry.path(), &dest)?;
		}

		// `ostree admin deploy` requires an (empty) /etc in the commit.
		std::fs::create_dir_all(&etc)?;

		Ok(())
	}

	/// Remove paths that must not be part of the committed tree.
	///
	/// These are runtime state or self-referential plumbing from the image build:
	/// the `sysroot/ostree/ostree -> /sysroot/ostree` symlink loop would make
	/// ostree recurse into itself, and `/var` state is not deployment content.
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

	/// Commit a rootfs into an OSTree repository.
	fn commit_rootfs(repo: &Path, rootfs: &Path, branch: &str, subject: &str) -> Result<()> {
		info!(?repo, ?branch, "Committing rootfs to OSTree repository");
		cmd_lib::run_cmd!(
			ostree commit
				--repo=$repo
				--branch=$branch
				--subject=$subject
				--tree=dir=$rootfs
				--owner-uid=0
				--owner-gid=0
				2>&1;
		)?;
		Ok(())
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
	fn deploy_sysroot(sysroot: &Path, stateroot: &str, branch: &str) -> Result<()> {
		let stateroot_dir = sysroot.join("ostree/deploy").join(stateroot);
		std::fs::create_dir_all(&stateroot_dir)?;
		std::fs::create_dir_all(sysroot.join("boot"))?;
		std::fs::create_dir_all(stateroot_dir.join("var"))?;

		info!(?sysroot, ?stateroot, ?branch, "Deploying OSTree commit into sysroot");
		cmd_lib::run_cmd!(
			ostree admin deploy --sysroot=$sysroot --os=$stateroot --stateroot=$stateroot $branch 2>&1;
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

		Self::sanitize_rootfs_for_commit(rootfs)?;
		Self::prepare_etc_for_commit(rootfs)?;

		if self.embed_image_metadata {
			let metadata =
				BootcImageMetadata { tag: image.to_string(), digest: digest.to_string() };
			let serialized = serde_yaml::to_string(&metadata)?;
			info!("Embedding image metadata into committed tree");
			std::fs::write(rootfs.join(".bootc_meta.yaml"), serialized)?;
		}

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
		let subject = format!(
			"{} {}",
			manifest.distro.as_deref().unwrap_or("Katsu"),
			image.split(':').next_back().unwrap_or("live")
		);
		Self::commit_rootfs(&repo, rootfs, &branch, &subject)?;

		// The mount is no longer needed now that the commit exists; dropping it
		// unmounts and removes the ephemeral container.
		drop(mounted);

		Self::deploy_sysroot(&sysroot, &self.stateroot, &branch)?;

		info!(?sysroot, ?branch, "OSTree sysroot ready for direct boot");
		Ok(TreeOutput::OstreeSysroot { sysroot, stateroot: self.stateroot.clone() })
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
