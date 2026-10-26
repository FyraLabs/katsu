use color_eyre::{
	Result,
	eyre::{bail, eyre},
};
use serde::{Deserialize, Serialize};
use std::{
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

use crate::config::Manifest;

mod grub;
mod limine;
mod refind;

/// An OSTree deployment, booted via its `ostree=` karg.
#[derive(Debug, Clone)]
struct OstreeDeployment {
	/// The `ostree=` kernel argument, e.g.
	/// `ostree=/ostree/boot.1/um/<bootcsum>/0`.
	pub karg: String,
}

/// Deployment karg and kernel/initramfs paths, so backends needn't know the layout.
#[derive(Debug, Clone, Default)]
pub struct BootPayload {
	/// The deployment-selecting kernel argument (`ostree=` or `composefs=`),
	/// empty for a plain rootfs.
	pub karg: String,
	/// Kernel path below the tree root, when the layout dictates it.
	pub vmlinuz: Option<PathBuf>,
	/// Initramfs path below the tree root, when the layout dictates it.
	pub initramfs: Option<PathBuf>,
}

impl BootPayload {
	/// Resolve whichever deployment layout the tree uses, if any.
	pub fn resolve(tree: &Path) -> Result<Self> {
		if let Some(ostree) = OstreeDeployment::resolve(tree)? {
			return Ok(Self { karg: ostree.karg, vmlinuz: None, initramfs: None });
		}
		if let Some(composefs) = ComposefsDeployment::resolve(tree)? {
			return Ok(Self {
				karg: composefs.karg,
				vmlinuz: Some(composefs.vmlinuz),
				initramfs: Some(composefs.initramfs),
			});
		}
		Ok(Self::default())
	}

	/// Initramfs marker gating the matching katsu live service; empty for a plain rootfs.
	pub fn live_marker(&self) -> &'static str {
		if self.karg.starts_with("composefs=") {
			"rd.katsu.composefs"
		} else if self.karg.starts_with("ostree=") {
			"rd.katsu.ostree"
		} else {
			""
		}
	}
}

/// A native composefs deployment, selected by `composefs=<SHA-512 verity digest>`.
/// Never reconstruct the digest; read it from the BLS entry.
#[derive(Debug, Clone)]
pub struct ComposefsDeployment {
	/// The `composefs=` kernel argument, e.g. `composefs=<verity digest>`.
	pub karg: String,
	/// Path of the kernel below the tree root.
	pub vmlinuz: PathBuf,
	/// Path of the initramfs below the tree root.
	pub initramfs: PathBuf,
}

impl ComposefsDeployment {
	/// Mount this deployment's image at `mountpoint`. A bare EROFS mount of a
	/// composefs image lacks file contents; it must be combined with `composefs/objects/`.
	pub fn mount(&self, repo: &Path, mountpoint: &Path) -> Result<()> {
		let digest = self.karg.trim_start_matches("composefs=").trim_start_matches('?');
		fs::create_dir_all(mountpoint)?;
		info!(?repo, digest, ?mountpoint, "Mounting composefs image");
		let repo: composefs::repository::Repository<composefs::fsverity::Sha512HashValue> =
			composefs::repository::Repository::open_path(rustix::fs::CWD, repo)
				.map_err(|e| eyre!("Opening composefs repository: {e:#}"))?;
		repo.mount_at(digest, mountpoint, &composefs::mount::MountOptions::default())
			.map_err(|e| eyre!("Mounting composefs image {digest}: {e:#}"))?;
		Ok(())
	}

	/// Read the deployment from bootc's BLS entry, the authoritative source.
	pub fn resolve(tree: &Path) -> Result<Option<Self>> {
		let entries_dir = tree.join("boot/loader/entries");
		let Ok(entries) = fs::read_dir(&entries_dir) else {
			return Ok(None);
		};

		// Only one deployment is ever installed, so the single BLS entry is ours.
		for entry in entries.flatten() {
			let path = entry.path();
			if path.extension().is_none_or(|e| e != "conf") {
				continue;
			}
			let Ok(contents) = fs::read_to_string(&path) else {
				continue;
			};

			let mut karg = None;
			let mut vmlinuz = None;
			let mut initramfs = None;
			for line in contents.lines() {
				let line = line.trim();
				if let Some(options) = line.strip_prefix("options ") {
					karg = options
						.split_whitespace()
						.find(|o| o.starts_with("composefs="))
						.map(|o| o.to_string());
				} else if let Some(linux) = line.strip_prefix("linux ") {
					vmlinuz = Some(linux.trim().trim_start_matches('/').to_string());
				} else if let Some(initrd) = line.strip_prefix("initrd ") {
					initramfs = Some(initrd.trim().trim_start_matches('/').to_string());
				}
			}

			let (Some(karg), Some(vmlinuz), Some(initramfs)) = (karg, vmlinuz, initramfs) else {
				debug!(?path, "BLS entry is not a composefs deployment; ignoring");
				continue;
			};

			let vmlinuz = tree.join(&vmlinuz);
			let initramfs = tree.join(&initramfs);
			if !vmlinuz.is_file() || !initramfs.is_file() {
				bail!(
					"Composefs BLS entry references missing files: {} / {}",
					vmlinuz.display(),
					initramfs.display()
				);
			}

			debug!(?path, %karg, "Resolved composefs deployment");
			return Ok(Some(Self { karg, vmlinuz, initramfs }));
		}

		Ok(None)
	}
}

impl OstreeDeployment {
	/// Resolve the OSTree deployment for a tree root, if it is a sysroot.
	///
	/// `tree` is either the deployment root (`ostree/deploy/<stateroot>/deploy/
	/// <csum>.0`) or an ostree sysroot. We detect the deployment root by walking
	/// up to the sysroot, then locate the matching boot-linkage directory under
	/// `ostree/boot.<n>/`.
	pub fn resolve(tree: &Path) -> Result<Option<Self>> {
		let Some((sysroot, stateroot, csum)) = Self::locate(tree) else {
			return Ok(None);
		};

		let Some(karg) = Self::find_ostree_karg(&sysroot, &stateroot, &csum)? else {
			warn!(
				?sysroot,
				"Found an OSTree deployment but no boot linkage; \
				 the system will not be able to locate it at boot"
			);
			return Ok(None);
		};

		debug!(?sysroot, %karg, "Resolved OSTree deployment");
		Ok(Some(Self { karg }))
	}

	/// Walk from either a sysroot or a deployment root to `(sysroot, stateroot, csum)`.
	fn locate(tree: &Path) -> Option<(PathBuf, String, String)> {
		// Case 1: `tree` is the deployment root itself.
		// <sysroot>/ostree/deploy/<stateroot>/deploy/<csum>.0
		if let Some(deploy_dir) = tree.parent()
			&& deploy_dir.file_name().is_some_and(|n| n == "deploy")
			&& let Some(stateroot_dir) = deploy_dir.parent()
			&& let Some(stateroot) =
				stateroot_dir.file_name().map(|n| n.to_string_lossy().to_string())
			&& let Some(deploy_base) = stateroot_dir.parent()
			// `sysroot/ostree/deploy` -> `sysroot/ostree` -> `sysroot`
			&& let Some(sysroot) = deploy_base.parent().and_then(|p| p.parent())
		{
			let csum = tree.file_name()?.to_string_lossy().to_string();
			return Some((sysroot.to_path_buf(), stateroot, csum));
		}

		// Case 2: `tree` is a sysroot; find the single deployed commit.
		let deploy_base = tree.join("ostree/deploy");
		for stateroot_entry in fs::read_dir(&deploy_base).ok()?.flatten() {
			let stateroot = stateroot_entry.file_name().to_string_lossy().to_string();
			let deploy_dir = stateroot_entry.path().join("deploy");
			if let Ok(deployment) = crate::backends::fs_tree::find_deployment(&deploy_dir) {
				let csum = deployment.file_name()?.to_string_lossy().to_string();
				return Some((tree.to_path_buf(), stateroot, csum));
			}
		}

		None
	}

	/// Read the `ostree=` karg whose boot-linkage symlink points at this deployment.
	fn find_ostree_karg(sysroot: &Path, stateroot: &str, csum: &str) -> Result<Option<String>> {
		let Some(entries_dir) = find_loader_entries_dir(&sysroot.join("boot")) else {
			debug!(?sysroot, "No boot loader entries directory found");
			return Ok(None);
		};
		let deployment = sysroot
			.join("ostree/deploy")
			.join(stateroot)
			.join("deploy")
			.join(csum)
			.canonicalize()?;

		for entry in fs::read_dir(&entries_dir)?.flatten() {
			let path = entry.path();
			if path.extension().is_none_or(|ext| ext != "conf") {
				continue;
			}
			let Ok(contents) = fs::read_to_string(&path) else {
				continue;
			};

			let Some(karg) = contents
				.lines()
				.find_map(|line| line.strip_prefix("options "))
				.and_then(|opts| opts.split_whitespace().find(|w| w.starts_with("ostree=")))
			else {
				warn!(?path, "BLS entry has no ostree= option");
				continue;
			};

			let linkage = sysroot.join(karg.trim_start_matches("ostree=/"));
			if linkage.canonicalize().is_ok_and(|target| target == deployment) {
				debug!(?path, %karg, "Read ostree karg from BLS entry");
				return Ok(Some(karg.to_string()));
			}
		}

		Ok(None)
	}
}

/// Locate ostree's active `loader/entries` (or the first `loader.<n>/entries`) under `boot_dir`.
pub(crate) fn find_loader_entries_dir(boot_dir: &Path) -> Option<PathBuf> {
	let linked = boot_dir.join("loader/entries");
	if linked.is_dir() {
		return Some(linked);
	}

	fs::read_dir(boot_dir)
		.ok()?
		.flatten()
		.filter(|e| e.file_name().to_string_lossy().starts_with("loader."))
		.map(|e| e.path().join("entries"))
		.find(|p| p.is_dir())
}

crate::prepend_comment!(GRUB_PREPEND_COMMENT: "/boot/grub/grub.cfg", "Grub configurations", katsu::builder::Bootloader::cp_grub);
crate::prepend_comment!(LIMINE_PREPEND_COMMENT: "/boot/limine.cfg", "Limine configurations", katsu::builder::Bootloader::cp_limine);
crate::prepend_comment!(REFIND_PREPEND_COMMENT: "/boot/efi/EFI/refind/refind.conf", "rEFInd configurations", katsu::builder::Bootloader::cp_refind);

/// Represents the bootloader types supported by Katsu
///
/// This enum defines the different bootloader implementations that can be used
/// when creating bootable images. Each variant corresponds to a specific
/// bootloader technology with its own installation and configuration methods.
#[derive(Default, Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Bootloader {
	#[default]
	/// Standard GRUB2 bootloader with UEFI support (default)
	Grub,
	/// GRUB2 bootloader configured for legacy BIOS systems
	GrubBios,
	/// Limine bootloader, a modern UEFI/BIOS bootloader
	Limine,
	/// systemd-boot, a simple UEFI boot manager
	SystemdBoot,
	/// rEFInd, a graphical UEFI boot manager
	REFInd,
}

impl From<&str> for Bootloader {
	fn from(value: &str) -> Self {
		match &*value.to_lowercase() {
			"limine" => Self::Limine,
			"grub" | "grub2" => Self::Grub,
			"grub-bios" => Self::GrubBios,
			"systemd-boot" => Self::SystemdBoot,
			"refind" => Self::REFInd,
			_ => {
				warn!("Unknown bootloader: {value}, falling back to GRUB");
				Self::Grub
			},
		}
	}
}

impl Bootloader {
	/// Installs the bootloader to the specified image
	///
	/// This method is responsible for actually installing the bootloader to the
	/// target image after it has been created. Different bootloaders require
	/// different installation procedures.
	///
	/// # Arguments
	///
	/// * `image` - The path to the image file where the bootloader will be installed
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn install(&self, image: &Path) -> Result<()> {
		match *self {
			Self::Grub => info!("GRUB is not required to be installed to image, skipping"),
			Self::Limine => cmd_lib::run_cmd!(limine bios-install $image 2>&1)?,
			Self::SystemdBoot => cmd_lib::run_cmd!(bootctl --image=$image install 2>&1)?,
			Self::GrubBios => {
				cmd_lib::run_cmd!(grub-install --target=i386-pc --boot-directory=$image/boot 2>&1)?
			},
			Self::REFInd => info!(
				"rEFInd doesn't need installation to ISO image, files already copied during ISO creation"
			),
		}
		Ok(())
	}
	/// Returns the paths to the UEFI and BIOS bootloader binaries
	///
	/// This method provides the relative paths to the bootloader binaries needed
	/// for creating bootable media. These paths are used during the ISO creation
	/// process to locate the appropriate files for UEFI and BIOS boot support.
	///
	/// # Returns
	///
	/// * A tuple of `(&'static str, &'static str)` containing:
	///   * First element: Path to the UEFI bootloader binary
	///   * Second element: Path to the BIOS bootloader binary
	pub fn get_bins(&self) -> (&'static str, &'static str) {
		match *self {
			Self::Grub => ("boot/efi/EFI/fedora/shim.efi", "boot/eltorito.img"),
			Self::Limine => ("boot/limine-uefi-cd.bin", "boot/limine-bios-cd.bin"),
			Self::GrubBios => todo!(),
			Self::SystemdBoot => todo!(),
			Self::REFInd => ("boot/efi/EFI/refind/refind_x64.efi", ""),
		}
	}
	/// Copies vmlinuz from /usr/lib/modules to `dest/boot/vmlinuz`.
	///
	/// # Arguments
	///
	/// * `chroot` - The path to the chroot directory containing the kernel
	/// * `dest` - The destination directory where vmlinuz should be copied
	///
	/// # Returns
	///
	/// * `Result<String>` - Success with kernel filename or failure with error details
	fn cp_vmlinuz_initramfs(&self, chroot: &Path, dest: &Path) -> Result<(String, String)> {
		trace!("Finding vmlinuz in /usr/lib/modules");

		// Prepare required directories
		fs::create_dir_all(dest.join("boot"))?;

		// Find kernel version and vmlinuz
		let (vmlinuz, kernel_version) = self.find_vmlinuz(chroot)?;
		debug!(?vmlinuz, ?kernel_version, "Kernel version and vmlinuz found");

		// Copy vmlinuz to destination
		let vmlinuz_dest = dest.join("boot").join("vmlinuz");
		trace!(?vmlinuz, ?vmlinuz_dest, "Copying vmlinuz to destination");

		let vmlinuz_src = if vmlinuz.is_empty() {
			bail!("Could not find vmlinuz path");
		} else {
			PathBuf::from(&vmlinuz)
		};

		if !vmlinuz_src.exists() {
			bail!("Source vmlinuz not found at {}", vmlinuz_src.display());
		}

		fs::copy(&vmlinuz_src, &vmlinuz_dest)?;

		Ok(("vmlinuz".to_string(), "initramfs.img".to_string()))
	}

	#[tracing::instrument(skip(self))]
	fn find_vmlinuz(&self, chroot: &Path) -> Result<(String, Option<String>)> {
		let modules_dir = chroot.join("usr/lib/modules");

		// Find kernel version from modules directory
		let mut kernels = fs::read_dir(&modules_dir)?;
		let kernel_version = kernels.find_map(|f| {
			trace!(?f, "File in /usr/lib/modules");
			f.ok().and_then(|entry| entry.file_name().to_str().map(|s| s.to_string()))
		});

		trace!("Kernel version found: {:?}", kernel_version);

		// Determine vmlinuz path based on kernel version
		let vmlinuz = if let Some(ref kernel_version) = kernel_version {
			modules_dir.join(kernel_version).join("vmlinuz").to_string_lossy().to_string()
		} else {
			// If no kernel version found, we'll try to find vmlinuz in boot directory later
			String::new()
		};

		Ok((vmlinuz, kernel_version))
	}

	#[tracing::instrument(skip(self))]
	#[allow(dead_code)]
	fn find_initramfs(&self, chroot: &Path) -> Result<PathBuf> {
		// Bootc images keep /boot empty and ship the initramfs next to the kernel.
		if let Some(path) = Self::find_initramfs_in_usr_lib_modules(chroot) {
			return Ok(path);
		}

		let bootdir = chroot.join("boot");

		// Search for initramfs in boot directory
		for f in bootdir.read_dir()? {
			let f = f?;
			if !f.metadata()?.is_file() {
				continue;
			}

			let name = f.file_name();
			debug!(?name, "File in /boot");
			let name = name.to_string_lossy();

			// Skip rescue images
			if name.contains("-rescue-") {
				continue;
			}

			// Look for initramfs files
			if name == "initramfs.img" || name.starts_with("initramfs-") {
				return Ok(f.path());
			}
		}

		bail!("Cannot find initramfs in {:?}", bootdir)
	}

	/// Look for an initramfs under /usr/lib/modules/<kver>/.
	fn find_initramfs_in_usr_lib_modules(chroot: &Path) -> Option<PathBuf> {
		let entries = fs::read_dir(chroot.join("usr/lib/modules")).ok()?;

		for entry in entries.flatten() {
			let dir = entry.path();
			if !dir.is_dir() {
				continue;
			}
			for candidate in ["initramfs.img", "initramfs"] {
				let path = dir.join(candidate);
				if path.is_file() {
					debug!(?path, "Found initramfs under /usr/lib/modules");
					return Some(path);
				}
			}

			// Any `initramfs*`/`initrd*` file in the module dir will do.
			if let Ok(files) = fs::read_dir(&dir) {
				for file in files.flatten() {
					let name = file.file_name();
					let name = name.to_string_lossy();
					if file.path().is_file()
						&& (name.starts_with("initramfs") || name.starts_with("initrd"))
						&& !name.contains("-rescue-")
					{
						let path = file.path();
						debug!(?path, "Found initramfs under /usr/lib/modules");
						return Some(path);
					}
				}
			}
		}

		None
	}

	#[tracing::instrument(skip(self))]
	#[allow(dead_code)]
	fn copy_boot_files(
		&self, chroot: &Path, dest: &Path, vmlinuz: &str, initramfs: &str,
	) -> Result<()> {
		let bootdir = chroot.join("boot");

		trace!(vmlinuz, initramfs, "Copying vmlinuz and initramfs");

		// Copy vmlinuz to destination
		let vmlinuz_dest = dest.join("boot").join("vmlinuz");
		trace!(?vmlinuz, ?vmlinuz_dest, "Copying vmlinuz to destination");
		let vmlinuz_src =
			if vmlinuz.is_empty() { bootdir.join("vmlinuz") } else { PathBuf::from(vmlinuz) };
		if !vmlinuz_src.exists() {
			bail!("Source vmlinuz not found at {}", vmlinuz_src.display());
		}
		fs::copy(&vmlinuz_src, &vmlinuz_dest)?;

		// Copy initramfs to destination
		let initramfs_src = bootdir.join(initramfs);
		let initramfs_dest = dest.join("boot").join("initramfs.img");
		if !initramfs_src.exists() {
			bail!("Source initramfs not found at {}", initramfs_src.display());
		}
		fs::copy(&initramfs_src, &initramfs_dest)?;

		// === start /boot cleanup ===
		if let Err(err) = fs::remove_file(&vmlinuz_src) {
			warn!(?err, path = %vmlinuz_src.display(), "Failed to remove source vmlinuz after copying");
		}
		if let Err(err) = fs::remove_file(&initramfs_src) {
			warn!(?err, path = %initramfs_src.display(), "Failed to remove source initramfs after copying");
		}

		// remove the rescue initramfs and vmlinuz if they exist
		let rescue_initramfs = bootdir.read_dir()?.find_map(|f| {
			let f = f.ok()?;
			let name = f.file_name().to_string_lossy().to_string();
			if name.contains("-rescue-") { Some(f.path()) } else { None }
		});

		if let Some(rescue_initramfs) = rescue_initramfs
			&& let Err(err) = fs::remove_file(&rescue_initramfs)
		{
			warn!(?err, path = %rescue_initramfs.display(), "Failed to remove rescue initramfs after copying");
		}

		let rescue_vmlinuz = bootdir.read_dir()?.find_map(|f| {
			let f = f.ok()?;
			let name = f.file_name().to_string_lossy().to_string();
			if name.contains("-rescue-") { Some(f.path()) } else { None }
		});

		if let Some(rescue_vmlinuz) = rescue_vmlinuz
			&& let Err(err) = fs::remove_file(&rescue_vmlinuz)
		{
			warn!(?err, path = %rescue_vmlinuz.display(), "Failed to remove rescue vmlinuz after copying");
		}

		// === end /boot cleanup ===

		Ok(())
	}

	/// Copies the bootloader files to the live OS image
	///
	/// This method copies all necessary bootloader files to the ISO tree to create
	/// a bootable live OS image. The specific files copied depend on the bootloader type.
	/// This is one of the main methods used during the ISO creation process.
	///
	/// # Arguments
	///
	/// * `manifest` - The manifest containing configuration information
	/// * `chroot` - The path to the chroot directory
	/// * `workspace` - The path to the workspace directory
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn copy_liveos(&self, manifest: &Manifest, chroot: &Path, workspace: &Path) -> Result<()> {
		info!("Copying bootloader files");
		let payload = BootPayload::resolve(chroot)?;
		if !payload.karg.is_empty() {
			info!(karg = %payload.karg, "Resolved deployment boot argument");
		}
		match *self {
			Self::Grub => self.cp_grub(manifest, chroot, workspace, &payload)?,
			Self::Limine => self.cp_limine(manifest, chroot, workspace, &payload)?,
			Self::SystemdBoot => todo!(),
			Self::GrubBios => self.cp_grub_bios(workspace)?,
			Self::REFInd => self.cp_refind(manifest, chroot, workspace, &payload)?,
		}
		Ok(())
	}

	/// Copies GRUB BIOS-specific files to the ISO tree
	///
	/// This method is responsible for setting up the legacy BIOS boot environment
	/// using GRUB. It's used when the bootloader type is GrubBios.
	///
	/// # Arguments
	///
	/// * `_workspace` - The path to the workspace directory
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn cp_grub_bios(&self, _workspace: &Path) -> Result<()> {
		todo!()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::{fs, os::unix::fs::symlink};

	const CSUM: &str = "0123456789abcdef";
	const BOOTCSUM: &str = "deadbeefcafe";
	const VERITY: &str = "c88912e35a047d05e878f3b690f9a411a15757e5f8b1931df5d3954a56a8c6dea0871ac4cf283eb5260a547c7e1ea2c797c734455aa79341bd01c14d57bd07e3";

	/// Build a minimal tree shaped like a native composefs install.
	fn make_composefs_tree(name: &str) -> PathBuf {
		let tree = std::env::temp_dir().join(name);
		let _ = fs::remove_dir_all(&tree);

		let boot = tree.join(format!("boot/bootc_composefs-{VERITY}"));
		fs::create_dir_all(&boot).unwrap();
		fs::write(boot.join("vmlinuz"), b"kernel").unwrap();
		fs::write(boot.join("initrd"), b"initrd").unwrap();

		let entries = tree.join("boot/loader/entries");
		fs::create_dir_all(&entries).unwrap();
		fs::write(
			entries.join("bootc_ultramarine-44-1.conf"),
			format!(
				"title Ultramarine Linux 44\n\
				 version 44\n\
				 linux /boot/bootc_composefs-{VERITY}/vmlinuz\n\
				 initrd /boot/bootc_composefs-{VERITY}/initrd\n\
				 options root=UUID=abc rw composefs={VERITY} quiet rhgb\n"
			),
		)
		.unwrap();
		tree
	}

	#[test]
	fn resolves_a_composefs_deployment_from_its_bls_entry() {
		let tree = make_composefs_tree("katsu-composefs-resolve");
		let boot = tree.join(format!("boot/bootc_composefs-{VERITY}"));

		let payload = BootPayload::resolve(&tree).unwrap();
		assert_eq!(payload.karg, format!("composefs={VERITY}"));
		assert_eq!(payload.vmlinuz, Some(boot.join("vmlinuz")));
		assert_eq!(payload.initramfs, Some(boot.join("initrd")));

		fs::remove_file(boot.join("vmlinuz")).unwrap();
		assert!(ComposefsDeployment::resolve(&tree).is_err());
	}

	#[test]
	fn boot_payload_is_empty_for_a_plain_rootfs() {
		let root = std::env::temp_dir().join("katsu-bootpayload-plain");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("usr/bin")).unwrap();

		let payload = BootPayload::resolve(&root).unwrap();
		assert!(payload.karg.is_empty());
		assert!(payload.vmlinuz.is_none());
	}

	/// Build a minimal sysroot shaped like the one `ostree admin deploy` produces.
	fn make_sysroot(name: &str) -> PathBuf {
		let sysroot = std::env::temp_dir().join(name);
		let _ = fs::remove_dir_all(&sysroot);

		let deploy = sysroot.join("ostree/deploy/um/deploy");
		fs::create_dir_all(deploy.join(format!("{CSUM}.0"))).unwrap();
		fs::write(deploy.join(format!("{CSUM}.0.origin")), b"[origin]\nrefspec=um/44/x86_64\n")
			.unwrap();

		let linkage = sysroot.join("ostree").join(format!("boot.1.1/um/{BOOTCSUM}"));
		fs::create_dir_all(&linkage).unwrap();
		symlink("boot.1.1", sysroot.join("ostree/boot.1")).unwrap();
		symlink(format!("../../../deploy/um/deploy/{CSUM}.0"), linkage.join("0")).unwrap();

		let entries = sysroot.join("boot/loader.1/entries");
		fs::create_dir_all(&entries).unwrap();
		fs::write(
			entries.join("ostree-1.conf"),
			format!(
				"title Ultramarine Linux 44\n\
				 version 1\n\
				 options ostree=/ostree/boot.1/um/{BOOTCSUM}/0 quiet\n\
				 linux /ostree/um-{BOOTCSUM}/vmlinuz\n\
				 initrd /ostree/um-{BOOTCSUM}/initramfs.img\n"
			),
		)
		.unwrap();

		sysroot
	}

	#[test]
	fn resolves_the_ostree_karg_from_a_sysroot_or_deployment_root() {
		let sysroot = make_sysroot("katsu-ostree-karg");
		let entries = sysroot.join("boot/loader.1/entries");
		// Another stateroot's entry must be ignored.
		fs::write(
			entries.join("ostree-0.conf"),
			"options ostree=/ostree/boot.1/gnome/othercommit/0\n\
			 linux /ostree/gnome-othercommit/vmlinuz\n",
		)
		.unwrap();
		// A stray sibling of the deployment, as a stale build would leave.
		fs::create_dir_all(sysroot.join("ostree/deploy/um/deploy/iso-tree")).unwrap();

		let rootfs = crate::backends::fs_tree::TreeOutput::OstreeSysroot {
			sysroot: sysroot.clone(),
			stateroot: "um".to_string(),
		}
		.rootfs()
		.unwrap();
		let expected = format!("ostree=/ostree/boot.1/um/{BOOTCSUM}/0");
		for tree in [&sysroot, &rootfs] {
			let deployment = OstreeDeployment::resolve(tree).unwrap().expect("should resolve");
			assert_eq!(deployment.karg, expected);
		}
	}

	#[test]
	fn matches_the_ostree_karg_by_linkage_not_serial() {
		let sysroot = make_sysroot("katsu-ostree-karg-serial");
		let other = sysroot.join("ostree/deploy/um/deploy/othercommit.0");
		fs::create_dir_all(&other).unwrap();
		symlink(
			"../../../deploy/um/deploy/othercommit.0",
			sysroot.join(format!("ostree/boot.1.1/um/{BOOTCSUM}/1")),
		)
		.unwrap();
		fs::write(
			sysroot.join("boot/loader.1/entries/ostree-2.conf"),
			format!("options ostree=/ostree/boot.1/um/{BOOTCSUM}/1\n"),
		)
		.unwrap();

		let deployment = OstreeDeployment::resolve(&other).unwrap().expect("should resolve");
		assert_eq!(deployment.karg, format!("ostree=/ostree/boot.1/um/{BOOTCSUM}/1"));
	}

	/// Bootc images leave /boot empty and ship the initramfs under /usr/lib/modules.
	#[test]
	fn finds_initramfs_under_usr_lib_modules() {
		let root = std::env::temp_dir().join("katsu-initramfs-modules");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("boot")).unwrap();
		let modules = root.join("usr/lib/modules/7.1.12-200.fc44.x86_64");
		fs::create_dir_all(&modules).unwrap();
		fs::write(modules.join("initramfs.img"), b"x").unwrap();

		let found = Bootloader::find_initramfs_in_usr_lib_modules(&root);

		assert_eq!(found, Some(modules.join("initramfs.img")));
	}

	/// legacy images use the flat /boot layout.
	#[test]
	fn finds_initramfs_in_boot() {
		let root = std::env::temp_dir().join("katsu-initramfs-boot");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("boot")).unwrap();
		fs::write(root.join("boot/initramfs-6.1.img"), b"x").unwrap();

		let found = Bootloader::default().find_initramfs(&root).unwrap();

		assert_eq!(found, root.join("boot/initramfs-6.1.img"));
	}

	/// Rescue initramfs images are not bootable targets for a live ISO.
	#[test]
	fn skips_rescue_initramfs_in_boot() {
		let root = std::env::temp_dir().join("katsu-initramfs-rescue");
		let _ = fs::remove_dir_all(&root);
		fs::create_dir_all(root.join("boot")).unwrap();
		fs::write(root.join("boot/initramfs-0-rescue-abc.img"), b"x").unwrap();
		fs::write(root.join("boot/initramfs-6.1.img"), b"x").unwrap();

		let found = Bootloader::default().find_initramfs(&root).unwrap();

		assert_eq!(found, root.join("boot/initramfs-6.1.img"));
	}
}
