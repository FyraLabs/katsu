use cmd_lib::{run_cmd, run_fun};
use color_eyre::{eyre::bail, Result};
use serde_derive::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::{
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

use crate::{
	builder::{BOOTIMGS, ISO_TREE},
	config::Manifest,
	util::loopdev_with_file,
};

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
			Self::REFInd => info!("rEFInd doesn't need installation to ISO image, files already copied during ISO creation"),
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
		std::fs::create_dir_all(dest.join("boot"))?;

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
			if name.contains("-rescue-") {
				Some(f.path())
			} else {
				None
			}
		});

		if let Some(rescue_initramfs) = rescue_initramfs {
			if let Err(err) = fs::remove_file(&rescue_initramfs) {
				warn!(?err, path = %rescue_initramfs.display(), "Failed to remove rescue initramfs after copying");
			}
		}

		let rescue_vmlinuz = bootdir.read_dir()?.find_map(|f| {
			let f = f.ok()?;
			let name = f.file_name().to_string_lossy().to_string();
			if name.contains("-rescue-") {
				Some(f.path())
			} else {
				None
			}
		});

		if let Some(rescue_vmlinuz) = rescue_vmlinuz {
			if let Err(err) = fs::remove_file(&rescue_vmlinuz) {
				warn!(?err, path = %rescue_vmlinuz.display(), "Failed to remove rescue vmlinuz after copying");
			}
		}

		// === end /boot cleanup ===

		Ok(())
	}

	fn cp_limine(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		// complaint to rust: why can't you coerce automatically with umwrap_or()????
		info!("Copying Limine files");
		let distro = &manifest.distro.as_ref().map_or("Linux", |s| s);
		let cmd = &manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let root = chroot.parent().unwrap().join(ISO_TREE);
		// std::fs::create_dir_all(format!("./{distro}/LiveOS"))?;
		std::fs::create_dir_all(root.join("boot"))?;
		std::fs::copy(
			"/usr/share/limine/limine-uefi-cd.bin",
			root.join("boot/limine-uefi-cd.bin"),
		)?;
		std::fs::copy(
			"/usr/share/limine/limine-bios-cd.bin",
			root.join("boot/limine-bios-cd.bin"),
		)?;
		std::fs::copy("/usr/share/limine/limine-bios.sys", root.join("boot/limine-bios.sys"))?;

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &root, false)?;
		let volid = manifest.get_volid();

		// Generate limine.cfg
		let limine_cfg = root.join("boot/limine.cfg");
		crate::tpl!("limine.cfg.tera" => { LIMINE_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &limine_cfg);

		let binding = run_fun!(b2sum $limine_cfg)?;
		let liminecfg_b2h = binding.split_whitespace().next().unwrap();

		// enroll limine secure boot
		tracing::info_span!("Enrolling Limine Secure Boot").in_scope(|| -> Result<()> {
			Ok(run_cmd!(
				limine enroll-config $root/boot/limine-uefi-cd.bin $liminecfg_b2h 2>&1;
				limine enroll-config $root/boot/limine-bios.sys $liminecfg_b2h 2>&1;
			)?)
		})?;

		Ok(())
	}

	fn cp_refind(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		info!("Copying rEFInd files");
		let distro = &manifest.distro.as_ref().map_or("Linux", |s| s);
		let cmd = &manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let iso_tree = chroot.parent().unwrap().join(ISO_TREE);

		std::fs::create_dir_all(iso_tree.join("EFI/BOOT"))?;

		std::fs::copy(
			"/usr/share/rEFInd/refind/refind_x64.efi",
			iso_tree.join("EFI/BOOT/BOOTX64.EFI"),
		)?;

		std::fs::create_dir_all(iso_tree.join("EFI/BOOT/drivers_x64"))?;

		std::fs::copy(
			"/usr/share/rEFInd/refind/drivers_x64/iso9660_x64.efi",
			iso_tree.join("EFI/BOOT/drivers_x64/iso9660_x64.efi"),
		)?;

		std::fs::copy(
			"/usr/share/rEFInd/refind/drivers_x64/ext4_x64.efi",
			iso_tree.join("EFI/BOOT/drivers_x64/ext4_x64.efi"),
		)?;

		std::fs::create_dir_all(iso_tree.join("EFI/BOOT/icons"))?;

		cmd_lib::run_cmd!(
			cp -rv /usr/share/rEFInd/refind/icons/. $iso_tree/EFI/BOOT/icons/ 2>&1;
		)?;

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &iso_tree, false)?;
		let volid = manifest.get_volid();

		let refind_cfg = iso_tree.join("EFI/BOOT/refind.conf");
		crate::tpl!("refind.cfg.tera" => { REFIND_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &refind_cfg);

		let mut nsh = std::fs::File::create(iso_tree.join("startup.nsh"))?;
		// Point directly to the rEFInd EFI file
		writeln!(nsh, "EFI\\BOOT\\BOOTX64.EFI")?;

		self.mk_refind_efiboot(chroot, manifest)?;

		Ok(())
	}

	/// Creates the rEFInd EFI boot image
	fn mk_refind_efiboot(&self, chroot: &Path, _: &Manifest) -> Result<()> {
		let tree = chroot.parent().unwrap().join(ISO_TREE);

		// make EFI disk
		let sparse_path = &tree.join("boot/efiboot.img");
		crate::util::create_sparse(sparse_path, 256 * 1024 * 1024)?; // 50MiB (increased from 25MiB)

		// let's mount the disk as a loop device
		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		cmd_lib::run_cmd!(
			// Format disk with mkfs.fat
			mkfs.msdos $ldp -v -n EFI 2>&1;

			// Mount disk to /tmp/katsu.efiboot
			mkdir -p /tmp/katsu.efiboot;
			mount $ldp /tmp/katsu.efiboot;

			mkdir -p /tmp/katsu.efiboot/EFI/BOOT;
			cp -avr $tree/EFI/BOOT/. /tmp/katsu.efiboot/EFI/BOOT 2>&1;

			// Copy kernel and initramfs to efiboot
			mkdir -p /tmp/katsu.efiboot/boot;
			cp -av $tree/boot/vmlinuz /tmp/katsu.efiboot/boot/ 2>&1;
			cp -av $tree/boot/initramfs.img /tmp/katsu.efiboot/boot/ 2>&1;

			umount /tmp/katsu.efiboot;
		)?;

		drop(hdl);
		Ok(())
	}

	/// A clone of mkefiboot from lorax
	/// Currently only works for PC, no mac support
	fn mkefiboot(&self, chroot: &Path, _: &Manifest) -> Result<()> {
		let tree = chroot.parent().unwrap().join(ISO_TREE);

		// TODO: Add mac boot support

		// make EFI disk
		let sparse_path = &tree.join("boot/efiboot.img");
		crate::util::create_sparse(sparse_path, 25 * 1024 * 1024)?; // 15MiB

		// let's mount the disk as a loop device
		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		cmd_lib::run_cmd!(
			// Format disk with mkfs.fat
			mkfs.msdos $ldp -v -n EFI 2>&1;

			// Mount disk to /tmp/katsu.efiboot
			mkdir -p /tmp/katsu.efiboot;
			mount $ldp /tmp/katsu.efiboot;

			mkdir -p /tmp/katsu.efiboot/EFI/BOOT;
			cp -avr $tree/EFI/BOOT/. /tmp/katsu.efiboot/EFI/BOOT 2>&1;

			umount /tmp/katsu.efiboot;
		)?;

		drop(hdl);
		Ok(())
	}

	fn cp_grub(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		let iso_tree = chroot.parent().unwrap().join(ISO_TREE);
		let boot_imgs_dir = chroot.parent().unwrap().join(BOOTIMGS);
		// create if not exist
		// port from katsu 0.9.2 :3
		std::fs::create_dir_all(&boot_imgs_dir)?; // create if not exist
		if self.get_arch(manifest) == "x86_64" {
			// Copy GRUB files for hybrid boot support
			info!("Copying GRUB hybrid boot image");
			let hybrid_img = chroot.join("usr/lib/grub/i386-pc/boot_hybrid.img");
			trace!(?hybrid_img, "Source hybrid boot image location");
			let dest = boot_imgs_dir.join("boot_hybrid.img");
			trace!(?dest, "Destination hybrid boot image location");
			if !hybrid_img.exists() {
				warn!("Hybrid boot image not found at expected location");
			}
			std::fs::copy(&hybrid_img, &dest)?;
			debug!("Successfully copied hybrid boot image");
		}

		// Create necessary directories
		self.create_grub_directories(&iso_tree, &boot_imgs_dir)?;

		// Prepare configuration variables
		let kernel_cmdline = manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let volid = manifest.get_volid();
		let distro = manifest.distro.as_ref().map_or("Linux", |s| s);

		// Copy kernel and initramfs
		let (vmlinuz, initramfs) =
			self.copy_kernel_and_initramfs(chroot, &boot_imgs_dir, &iso_tree)?;

		// Generate GRUB configuration
		self.generate_grub_config(&iso_tree, volid, distro, &vmlinuz, &initramfs, kernel_cmdline)?;

		// Set up EFI boot files
		self.setup_efi_boot_files(manifest, &iso_tree)?;

		// Generate GRUB images
		self.generate_grub_images(chroot, &iso_tree, manifest)?;

		// Create EFI boot image
		self.mkefiboot(chroot, manifest)?;

		Ok(())
	}

	fn create_grub_directories(&self, iso_tree: &Path, boot_imgs_dir: &Path) -> Result<()> {
		std::fs::create_dir_all(iso_tree)?;
		std::fs::create_dir_all(boot_imgs_dir)?;
		Ok(())
	}

	fn copy_kernel_and_initramfs(
		&self, chroot: &Path, boot_imgs_dir: &Path, iso_tree: &Path,
	) -> Result<(String, String)> {
		// Copy vmlinuz and initramfs to bootimgs directory
		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, boot_imgs_dir, true)?;

		let iso_boot = iso_tree.join("boot");
		let chroot_boot = chroot.join("boot");

		// Clean existing boot directory if present and recreate minimal structure
		let _ = std::fs::remove_dir_all(&iso_boot);
		std::fs::create_dir_all(&iso_boot)?;

		let grub_dest = iso_boot.join("grub");
		let grub2_src = chroot_boot.join("grub2");
		let grub_src = chroot_boot.join("grub");
		let _ = std::fs::remove_dir_all(&grub_dest);
		if grub2_src.exists() {
			Self::copy_dir(&grub2_src, &grub_dest)?;
		} else if grub_src.exists() {
			Self::copy_dir(&grub_src, &grub_dest)?;
		} else {
			bail!("Missing grub directory in {}", chroot_boot.display());
		}

		let efi_src = chroot_boot.join("efi");
		let efi_dest = iso_boot.join("efi");
		let _ = std::fs::remove_dir_all(&efi_dest);
		if efi_src.exists() {
			Self::copy_dir(&efi_src, &efi_dest)?;
		} else {
			warn!("No EFI directory found in {}", chroot_boot.display());
		}

		// Copy vmlinuz and initramfs from bootimgs to ISO tree
		std::fs::copy(boot_imgs_dir.join("boot").join(&vmlinuz), iso_boot.join(&vmlinuz))?;

		std::fs::copy(boot_imgs_dir.join("boot").join(&initramfs), iso_boot.join("initramfs.img"))?;

		Ok((vmlinuz, "initramfs.img".to_string()))
	}

	fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
		if !src.exists() {
			bail!("Source directory {} does not exist", src.display());
		}
		if dest.exists() {
			std::fs::remove_dir_all(dest)?;
		}
		std::fs::create_dir_all(dest)?;

		for entry in std::fs::read_dir(src)? {
			let entry = entry?;
			let entry_path = entry.path();
			let dest_path = dest.join(entry.file_name());
			let file_type = std::fs::symlink_metadata(&entry_path)?.file_type();
			if file_type.is_dir() {
				Self::copy_dir(&entry_path, &dest_path)?;
			} else if file_type.is_file() {
				std::fs::copy(&entry_path, &dest_path)?;
			} else if file_type.is_symlink() {
				let target = std::fs::read_link(&entry_path)?;
				{
					symlink(target, &dest_path)?;
				}
			}
		}

		Ok(())
	}

	fn generate_grub_config(
		&self, iso_tree: &Path, volid: String, distro: &str, vmlinuz: &str, initramfs: &str,
		kernel_cmdline: &str,
	) -> Result<()> {
		// Generate grub.cfg using template
		crate::tpl!(
			"grub.cfg.tera" => {
				GRUB_PREPEND_COMMENT,
				volid,
				distro,
				vmlinuz: vmlinuz.to_string(),
				initramfs: initramfs.to_string(),
				cmd: kernel_cmdline.to_string()
			} => iso_tree.join("boot/grub/grub.cfg")
		);

		Ok(())
	}

	fn setup_efi_boot_files(&self, manifest: &Manifest, iso_tree: &Path) -> Result<()> {
		// Determine architecture-specific values
		let arch_short = self.get_arch_short(manifest);
		let arch_short_upper = arch_short.to_uppercase();
		let arch_32 = self.get_arch_32bit(manifest).to_uppercase();

		// Create EFI directories
		std::fs::create_dir_all(iso_tree.join("EFI/BOOT/fonts"))?;

		// Copy and configure EFI files
		cmd_lib::run_cmd!(
			cp -av $iso_tree/boot/efi/EFI/fedora/. $iso_tree/EFI/BOOT;
			cp -av $iso_tree/boot/grub/grub.cfg $iso_tree/EFI/BOOT/BOOT.conf 2>&1;
			cp -av $iso_tree/boot/grub/grub.cfg $iso_tree/EFI/BOOT/grub.cfg 2>&1;
			cp -av $iso_tree/boot/grub/fonts/unicode.pf2 $iso_tree/EFI/BOOT/fonts;
			cp -av $iso_tree/EFI/BOOT/shim${arch_short}.efi $iso_tree/EFI/BOOT/BOOT${arch_short_upper}.efi;
			cp -av $iso_tree/EFI/BOOT/shim.efi $iso_tree/EFI/BOOT/BOOT${arch_32}.efi;
		)?;

		Ok(())
	}

	fn get_arch<'a>(&self, manifest: &'a Manifest) -> &'a str {
		manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH)
	}

	fn get_arch_short(&self, manifest: &Manifest) -> &'static str {
		match self.get_arch(manifest) {
			"x86_64" => "x64",
			"aarch64" => "aa64",
			_ => unimplemented!(),
		}
	}

	fn get_arch_32bit(&self, manifest: &Manifest) -> &'static str {
		match self.get_arch(manifest) {
			"x86_64" => "ia32",
			"aarch64" => "arm",
			_ => unimplemented!(),
		}
	}

	fn generate_grub_images(
		&self, chroot: &Path, iso_tree: &Path, manifest: &Manifest,
	) -> Result<()> {
		let host_arch = std::env::consts::ARCH;
		let target_arch = manifest.dnf.arch.as_deref().unwrap_or(host_arch);

		let arch = match target_arch {
			"x86_64" => "i386-pc",
			"aarch64" => "arm64-efi",
			_ => unimplemented!(),
		};

		let arch_out = match target_arch {
			"x86_64" => "i386-pc-eltorito",
			"aarch64" => "arm64-efi",
			_ => unimplemented!(),
		};

		let arch_modules = match target_arch {
			"x86_64" => vec!["biosdisk"],
			"aarch64" => vec!["efi_gop"],
			_ => unimplemented!(),
		};

		debug!("Generating Grub images");
		cmd_lib::run_cmd!(
			// Create eltorito.img for ISO boot
			grub2-mkimage -O $arch_out -d $chroot/usr/lib/grub/$arch -o $iso_tree/boot/eltorito.img -p /boot/grub iso9660 $[arch_modules] 2>&1;

			// Create rescue image for EFI files
			grub2-mkrescue -o $iso_tree/../efiboot.img;
		)?;

		debug!("Copying EFI files from Grub rescue image");
		let (loop_device, handle) = loopdev_with_file(&iso_tree.join("../efiboot.img"))?;

		cmd_lib::run_cmd!(
			mkdir -p /tmp/katsu-efiboot;
			mount $loop_device /tmp/katsu-efiboot;
			cp -r /tmp/katsu-efiboot/boot/grub $iso_tree/boot/;
			umount /tmp/katsu-efiboot;
		)?;

		drop(handle);

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
