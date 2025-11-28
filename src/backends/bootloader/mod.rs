use color_eyre::{Result, eyre::bail};
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
	/// Copies vmlinuz (and optionally initramfs) from /usr/lib/modules to destination
	///
	/// This helper method locates the kernel (vmlinuz) file in /usr/lib/modules
	/// and copies it to the destination directory. When requested, it will also
	/// copy the initramfs image from the chroot's `/boot` into the destination,
	/// normalising the name to `initramfs.img` so the rest of the ISO generation
	/// pipeline can rely on a consistent filename.
	///
	/// # Arguments
	///
	/// * `chroot` - The path to the chroot directory containing the kernel
	/// * `dest` - The destination directory where vmlinuz should be copied
	///
	/// # Returns
	///
	/// * `Result<String>` - Success with kernel filename or failure with error details
	fn cp_vmlinuz_initramfs(
		&self, chroot: &Path, dest: &Path, copy_initramfs: bool,
	) -> Result<(String, String)> {
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

		if copy_initramfs {
			let initramfs_name = self.find_initramfs(chroot)?;
			let initramfs_src = chroot.join("boot").join(&initramfs_name);
			let initramfs_dest = dest.join("boot").join("initramfs.img");
			trace!(?initramfs_src, ?initramfs_dest, "Copying initramfs to destination");

			if !initramfs_src.exists() {
				bail!("Source initramfs not found at {}", initramfs_src.display());
			}

			fs::copy(&initramfs_src, &initramfs_dest)?;
		}

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
	fn find_initramfs(&self, chroot: &Path) -> Result<String> {
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
				return Ok(name.to_string());
			}
		}

		bail!("Cannot find initramfs in {:?}", bootdir)
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

		if let Some(rescue_initramfs) = rescue_initramfs {
			if let Err(err) = fs::remove_file(&rescue_initramfs) {
				warn!(?err, path = %rescue_initramfs.display(), "Failed to remove rescue initramfs after copying");
			}
		}

		let rescue_vmlinuz = bootdir.read_dir()?.find_map(|f| {
			let f = f.ok()?;
			let name = f.file_name().to_string_lossy().to_string();
			if name.contains("-rescue-") { Some(f.path()) } else { None }
		});

		if let Some(rescue_vmlinuz) = rescue_vmlinuz {
			if let Err(err) = fs::remove_file(&rescue_vmlinuz) {
				warn!(?err, path = %rescue_vmlinuz.display(), "Failed to remove rescue vmlinuz after copying");
			}
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
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn copy_liveos(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		info!("Copying bootloader files");
		match *self {
			Self::Grub => self.cp_grub(manifest, chroot)?,
			Self::Limine => self.cp_limine(manifest, chroot)?,
			Self::SystemdBoot => todo!(),
			Self::GrubBios => self.cp_grub_bios(chroot)?,
			Self::REFInd => self.cp_refind(manifest, chroot)?,
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
	/// * `_chroot` - The path to the chroot directory
	///
	/// # Returns
	///
	/// * `Result<()>` - Success or failure with error details
	pub fn cp_grub_bios(&self, _chroot: &Path) -> Result<()> {
		todo!()
	}
}
