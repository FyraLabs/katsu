pub mod dnf;
pub mod oci;

use crate::config::Manifest;
use color_eyre::{
	Result,
	eyre::{WrapErr, bail},
};
use nix::mount::{MntFlags, MsFlags};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// File name of the staging disk image bootc installs a unified layout into,
/// relative to the workspace.
pub const UNIFIED_STAGING_IMAGE: &str = "unified.raw";

pub trait RootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<TreeOutput>;
}

/// What a [`RootBuilder`] produces. Sysroot variants are not plain rootfs trees
/// (content lives under a deployment or composefs repo, and boot metadata comes
/// from ostree/bootc), so phases must branch on this.
#[allow(dead_code)]
#[derive(Debug)]
pub enum TreeOutput {
	// tar stream, unused atm
	Tarball(PathBuf),
	/// A plain root filesystem.
	Directory(PathBuf),
	/// An OSTree sysroot containing a repository and a bootable deployment.
	OstreeSysroot {
		/// The sysroot root, i.e. the directory containing `ostree/repo`.
		sysroot: PathBuf,
		/// The stateroot (ostree `--os`) the deployment lives under.
		stateroot: String,
	},
	/// A bootc composefs unified-storage root: `composefs/`, `state/` and `boot/`
	/// at the top level, booted with `composefs=<D>` from the BLS entry.
	///
	/// holders keep the mount and loop device alive until drop.
	UnifiedSysroot {
		sysroot: PathBuf,
		/// The staging disk image bootc installed into, whose ESP the live payload
		/// must carry so bootc can find it at runtime.
		staging_image: PathBuf,
		/// Keeps the loop device attached while the layers below are mounted.
		_loop: crate::util::LoopDevHdl,
		/// The overlay and its read-only lower layer, torn down on drop.
		_mounts: Vec<StagedRoot>,
	},
}

impl TreeOutput {
	/// The path that should be treated as the root filesystem for this output.
	///
	/// For a plain directory this is the directory itself. For a sysroot this is
	/// the *deployment* root, so that callers which only need to read or modify
	/// files (bootloader copying, dracut, inspection) do not have to know about
	/// the ostree layout.
	pub fn rootfs(&self) -> Result<PathBuf> {
		match self {
			Self::Directory(path) => Ok(path.clone()),
			Self::OstreeSysroot { sysroot, stateroot } => {
				let deploy_dir = sysroot.join("ostree/deploy").join(stateroot).join("deploy");
				find_deployment(&deploy_dir)
			},
			Self::Tarball(_) => {
				bail!("Tarball output has no root filesystem; this code path is unimplemented")
			},
			// Unlike OSTree, the composefs root *is* the tree: there is no
			// deployment subdirectory to descend into.
			Self::UnifiedSysroot { sysroot, .. } => Ok(sysroot.clone()),
		}
	}

	/// The directory that should be squashed into the live image.
	///
	/// This is deliberately *not* the same as [`Self::rootfs`]: for a sysroot the
	/// whole sysroot must ship, because the repository it contains is what makes
	/// the deployment bootable.
	pub fn squash_root(&self) -> PathBuf {
		match self {
			Self::Directory(path)
			| Self::Tarball(path)
			| Self::OstreeSysroot { sysroot: path, .. }
			| Self::UnifiedSysroot { sysroot: path, .. } => path.clone(),
		}
	}

	/// Whether this output is a sysroot (OSTree or composefs) rather than a plain rootfs.
	pub fn is_sysroot(&self) -> bool {
		matches!(self, Self::OstreeSysroot { .. } | Self::UnifiedSysroot { .. })
	}

	/// The staging disk image a unified layout was installed into, if any.
	///
	/// The live payload has to carry that image's ESP, because bootc looks for one
	/// on the media it boots from; a plain EROFS loop would not provide it.
	pub fn staging_image(&self) -> Option<&Path> {
		match self {
			Self::UnifiedSysroot { staging_image, .. } => Some(staging_image),
			_ => None,
		}
	}

	/// Reconstruct a `TreeOutput` from a workspace left by a previous build, so the
	/// `root` phase can be skipped when iterating on later phases.
	///
	/// Returns `None` unless the tree looks complete: a build killed during
	/// `ostree commit` leaves a repo with no deployments, and reusing that would
	/// only fail much later.
	pub fn from_existing(workspace: &Path) -> Result<Option<Self>> {
		// A unified tree is a mount over the staging image, so rebuild it from the image.
		if workspace.join(UNIFIED_STAGING_IMAGE).is_file() {
			info!(?workspace, "Re-attaching unified composefs sysroot from its staging image");
			return Self::mount_unified(workspace).map(Some);
		}

		let sysroot = workspace.join("bootc-sysroot");
		if !sysroot.is_dir() {
			debug!(?sysroot, "No existing sysroot to reuse");
			return Ok(None);
		}

		// A native composefs tree is recognised by its repository, and is not an
		// OSTree sysroot: it has no `ostree/deploy` stateroot.
		if sysroot.join("composefs").is_dir() {
			bail!(
				"found a composefs tree at {} but no staging image to back it; \
				 the unified layout needs a full `root` phase",
				sysroot.display()
			);
		}

		Self::reuse_ostree_sysroot(&sysroot)
	}

	/// Mount the unified staging image's root partition at `bootc-sysroot`.
	///
	/// Used both right after the bootc install and to re-attach a workspace left by
	/// a previous `root` phase.
	pub(crate) fn mount_unified(workspace: &Path) -> Result<Self> {
		let disk = workspace.join(UNIFIED_STAGING_IMAGE);
		let (loop_dev, loop_hdl) = crate::util::loopdev_with_file_and_parts(&disk)?;
		// The partition nodes may need a moment to appear after a scan.
		cmd_lib::run_cmd!(udevadm settle;)?;
		let part = PathBuf::from(format!("{}p3", loop_dev.display()));

		let staging = workspace.join("unified-root");
		fs::create_dir_all(&staging)?;
		nix::mount::mount(Some(&part), &staging, Some("btrfs"), MsFlags::empty(), None::<&str>)
			.wrap_err_with(|| format!("mounting {} at {}", part.display(), staging.display()))?;
		let staging_guard = StagedRoot::new(staging.clone());

		// Bind-mount, not overlay: overlayfs treats the graphroot's 0:0 char-device
		// whiteouts as its own and hides them, which breaks `mkfs.erofs`'s xattr pass.
		let sysroot = workspace.join("bootc-sysroot");
		fs::create_dir_all(&sysroot)?;
		nix::mount::mount(Some(&staging), &sysroot, None::<&str>, MsFlags::MS_BIND, None::<&str>)
			.wrap_err_with(|| format!("binding {} at {}", staging.display(), sysroot.display()))?;

		// Build the guard before validating so a bail does not leak the mounts.
		let output = Self::UnifiedSysroot {
			sysroot: sysroot.clone(),
			staging_image: disk,
			_loop: loop_hdl,
			_mounts: vec![StagedRoot::new(sysroot), staging_guard],
		};
		if !output.squash_root().join("composefs").is_dir() {
			bail!("staging image has no composefs repository; re-run the `root` phase");
		}
		Ok(output)
	}

	fn reuse_ostree_sysroot(sysroot: &Path) -> Result<Option<Self>> {
		let deploy_base = sysroot.join("ostree/deploy");
		let Ok(stateroot_entries) = fs::read_dir(&deploy_base) else {
			debug!(?deploy_base, "Existing sysroot has no stateroot; not reusing");
			return Ok(None);
		};

		// BLS entries only exist after a successful deploy.
		if !Self::has_boot_entries(sysroot) {
			warn!(?sysroot, "Existing sysroot has no boot entries; treating as incomplete");
			return Ok(None);
		}

		// Discover the stateroot rather than assuming one, so a manifest change
		// cannot silently reuse an unrelated tree.
		for entry in stateroot_entries.flatten() {
			let stateroot = entry.file_name().to_string_lossy().into_owned();
			let deploy_dir = entry.path().join("deploy");

			let Ok(deployment) = find_deployment(&deploy_dir) else {
				continue;
			};

			if !deployment.join("usr").is_dir() {
				warn!(
					?deployment,
					"Existing deployment has no /usr; treating sysroot as incomplete"
				);
				continue;
			}

			info!(?sysroot, ?stateroot, "Reusing existing OSTree sysroot");
			return Ok(Some(Self::OstreeSysroot { sysroot: sysroot.to_path_buf(), stateroot }));
		}

		Ok(None)
	}

	/// Whether the sysroot has any BLS entries under `boot/loader.<n>/entries`.
	fn has_boot_entries(sysroot: &Path) -> bool {
		let boot = sysroot.join("boot");
		let candidates = std::iter::once(boot.join("loader/entries")).chain(
			fs::read_dir(&boot)
				.into_iter()
				.flatten()
				.flatten()
				.filter(|e| e.file_name().to_string_lossy().starts_with("loader."))
				.map(|e| e.path().join("entries")),
		);

		candidates.filter_map(|dir| fs::read_dir(dir).ok()).any(|entries| {
			entries.flatten().any(|e| e.path().extension().is_some_and(|x| x == "conf"))
		})
	}
}

/// Keeps a mount attached for as long as its owner lives.
#[derive(Debug)]
pub struct StagedRoot {
	pub(crate) mount: PathBuf,
}

impl StagedRoot {
	pub(crate) fn new(mount: PathBuf) -> Self {
		Self { mount }
	}
}

impl Drop for StagedRoot {
	fn drop(&mut self) {
		if let Err(err) = nix::mount::umount2(&self.mount, MntFlags::empty()) {
			debug!(?err, mount = %self.mount.display(), "Unmounting staged root failed");
		}
	}
}

/// Find the (single) deployment directory under a stateroot's `deploy` dir.
///
/// Deployments are named `<commit checksum>.0`; we pick the first one, which is
/// the booted/default deployment because we only ever deploy a single commit.
pub fn find_deployment(deploy_dir: &Path) -> Result<PathBuf> {
	let entries = fs::read_dir(deploy_dir)
		.wrap_err_with(|| format!("Reading deployments from {}", deploy_dir.display()))?;

	for entry in entries.flatten() {
		let path = entry.path();
		let name = entry.file_name();
		let name = name.to_string_lossy();
		// Ignore the `.origin` sidecar files ostree writes next to deployments.
		if path.is_dir() && name.ends_with(".0") {
			return Ok(path);
		}
	}

	bail!("No deployment found under {}", deploy_dir.display())
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

	/// Lay out a sysroot that looks like a finished bootc build.
	fn write_complete_sysroot(workspace: &Path, stateroot: &str) {
		let sysroot = workspace.join("bootc-sysroot");
		let deploy = sysroot.join("ostree/deploy").join(stateroot).join("deploy");
		let deployment = deploy.join("0123456789abcdef.0");
		fs::create_dir_all(deployment.join("usr/bin")).unwrap();

		let entries = sysroot.join("boot/loader.1/entries");
		fs::create_dir_all(&entries).unwrap();
		fs::write(entries.join("ostree-1.conf"), b"title test\n").unwrap();
	}

	#[test]
	fn reuses_a_complete_sysroot() {
		let workspace = scratch("katsu-treeoutput-reuse");
		write_complete_sysroot(&workspace, "um");

		let output = TreeOutput::from_existing(&workspace).unwrap().expect("should reuse");

		assert!(output.is_sysroot());
		assert_eq!(
			output.rootfs().unwrap(),
			workspace.join("bootc-sysroot/ostree/deploy/um/deploy/0123456789abcdef.0")
		);
	}

	/// A build killed during `ostree commit` leaves a repo with no deployments.
	/// Reusing that would produce a broken image much later, so it must be refused.
	#[test]
	fn refuses_a_sysroot_with_no_deployment() {
		let workspace = scratch("katsu-treeoutput-nodeploy");
		fs::create_dir_all(workspace.join("bootc-sysroot/ostree/repo")).unwrap();

		assert!(TreeOutput::from_existing(&workspace).unwrap().is_none());
	}

	/// A truncated deployment (no /usr) is not usable.
	#[test]
	fn refuses_a_deployment_without_usr() {
		let workspace = scratch("katsu-treeoutput-nousr");
		write_complete_sysroot(&workspace, "um");
		let deployment = workspace.join("bootc-sysroot/ostree/deploy/um/deploy/0123456789abcdef.0");
		fs::remove_dir_all(deployment.join("usr")).unwrap();

		assert!(TreeOutput::from_existing(&workspace).unwrap().is_none());
	}

	/// A deployment with no BLS entries means the deploy step never completed.
	#[test]
	fn refuses_a_sysroot_without_boot_entries() {
		let workspace = scratch("katsu-treeoutput-noentries");
		write_complete_sysroot(&workspace, "um");
		fs::remove_dir_all(workspace.join("bootc-sysroot/boot")).unwrap();

		assert!(TreeOutput::from_existing(&workspace).unwrap().is_none());
	}

	#[test]
	fn discovers_the_stateroot_rather_than_assuming_one() {
		let workspace = scratch("katsu-treeoutput-stateroot");
		write_complete_sysroot(&workspace, "custom");

		let output = TreeOutput::from_existing(&workspace).unwrap().expect("should reuse");

		match output {
			TreeOutput::OstreeSysroot { stateroot, .. } => assert_eq!(stateroot, "custom"),
			other => panic!("expected a sysroot, got {other:?}"),
		}
	}
}
