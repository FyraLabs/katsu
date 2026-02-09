use super::{Bootloader, GRUB_PREPEND_COMMENT};
use crate::{
	builder::{BOOTIMGS, ISO_TREE},
	config::Manifest,
	util::loopdev_with_file,
};
use color_eyre::{Result, eyre::bail};
use std::{fs, os::unix::fs::symlink, path::Path};
use tracing::{debug, info, info_span, trace, warn};

impl Bootloader {
	pub(super) fn cp_grub(
		&self, manifest: &Manifest, chroot: &Path, workspace: &Path,
	) -> Result<()> {
		let iso_tree = workspace.join(ISO_TREE);
		let boot_imgs_dir = workspace.join(BOOTIMGS);

		fs::create_dir_all(&boot_imgs_dir)?;
		if self.get_arch(manifest) == "x86_64" {
			info!("Copying GRUB hybrid boot image");
			let hybrid_img = chroot.join("usr/lib/grub/i386-pc/boot_hybrid.img");
			trace!(?hybrid_img, "Source hybrid boot image location");
			let dest = boot_imgs_dir.join("boot_hybrid.img");
			trace!(?dest, "Destination hybrid boot image location");
			if !hybrid_img.exists() {
				warn!("Hybrid boot image not found at expected location");
			}
			fs::copy(&hybrid_img, &dest)?;
			debug!("Successfully copied hybrid boot image");
		}

		self.create_grub_directories(&iso_tree, &boot_imgs_dir)?;

		let kernel_cmdline = manifest.kernel_cmdline.as_deref().unwrap_or("");
		let volid = manifest.get_volid();
		let distro = manifest.distro.as_deref().unwrap_or("Linux");

		let (vmlinuz, initramfs) =
			self.copy_kernel_and_initramfs(chroot, &boot_imgs_dir, &iso_tree)?;

		self.generate_grub_config(&iso_tree, volid, distro, &vmlinuz, &initramfs, kernel_cmdline)?;
		self.setup_efi_boot_files(manifest, &iso_tree)?;
		self.generate_grub_images(chroot, &iso_tree, manifest)?;
		self.mkefiboot(workspace, manifest)?;

		Ok(())
	}

	fn create_grub_directories(&self, iso_tree: &Path, boot_imgs_dir: &Path) -> Result<()> {
		fs::create_dir_all(iso_tree)?;
		fs::create_dir_all(boot_imgs_dir)?;
		Ok(())
	}

	fn copy_kernel_and_initramfs(
		&self, chroot: &Path, boot_imgs_dir: &Path, iso_tree: &Path,
	) -> Result<(String, String)> {
		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, boot_imgs_dir, true)?;

		let iso_boot = iso_tree.join("boot");
		let chroot_boot = if chroot.join("usr/lib/ostree-boot").exists() {
			info!("Detected ostree-boot structure");
			chroot.join("usr/lib/ostree-boot")
		} else {
			chroot.join("boot")
		};
		// HACK: detect ostree-boot to avoid copying wrong files
		let ostree_boot = chroot_boot.ends_with("ostree-boot");

		let _ = fs::remove_dir_all(&iso_boot);
		fs::create_dir_all(&iso_boot)?;

		let grub_dest = iso_boot.join("grub");
		let grub2_src = chroot_boot.join("grub2");
		let grub_src = chroot_boot.join("grub");
		let grub_lib_dir = chroot.join("usr/lib/grub");
		let _ = fs::remove_dir_all(&grub_dest);
		if grub2_src.exists() {
			Self::copy_dir(&grub2_src, &grub_dest)?;
		} else if grub_src.exists() {
			Self::copy_dir(&grub_src, &grub_dest)?;
		} else if grub_lib_dir.exists() {
			warn!(
				"Grub directory not found in boot directory, but usr/lib/grub exists. Copying from there."
			);
			Self::copy_dir(&grub_lib_dir, &grub_dest)?;
			// Copy fonts to proper font directory
			let fonts_src = chroot.join("usr/share/grub");
			if fonts_src.exists() {
				let fonts_dest = grub_dest.join("fonts");
				Self::copy_dir(&fonts_src, &fonts_dest)?;
			} else {
				warn!("No grub fonts directory found in {}", fonts_src.display());
			}
		} else {
			bail!("Missing grub directory in {}", chroot_boot.display());
		}

		let efi_src = chroot_boot.join("efi");
		let efi_dest = iso_boot.join("efi");
		let _ = fs::remove_dir_all(&efi_dest);
		info_span!("Copying EFI boot files").in_scope(|| {
			info!(?efi_src, ?efi_dest, "Copying EFI boot files");
			// funny legacy boot path in case no one has cleaned out /boot/efi
			if chroot.join("boot/efi").exists() {
				Self::copy_dir(&efi_src, &efi_dest)?;
			}
			else if ostree_boot {
				warn!("bootupd detected, attempting to copying files from /usr/lib/efi");

				let libefi = chroot.join("usr/lib/efi");
				if libefi.exists() {
					for entry in fs::read_dir(&libefi)? {
						// /usr/lib/efi will contain <package_name>/<version> directories
						// which then contains the EFI files relative to /boot
						// so something like `shim/1.0/EFI/BOOT/fbx64.efi`
						// we would copy into the ISO tree's /boot/efi as /boot/efi/EFI/BOOT/fbx64.efi
						let entry = entry?;
						let entry_path = entry.path();
						info!(?entry_path, "Processing entry in /usr/lib/efi");
						if entry_path.is_dir() {
							// Read version directory
							for version_entry in fs::read_dir(&entry_path)? {
								info!(
									?version_entry,
									"Processing version directory in /usr/lib/efi"
								);
								let version_entry = version_entry?;
								let version_path = version_entry.path();

								if version_path.is_dir() {
									let efi_subsrc = version_path.join("EFI");
									let efi_dest_subdir = efi_dest.join("EFI");
									if efi_subsrc.exists() {
										debug!(
											?efi_subsrc,
											?efi_dest_subdir,
											"Copying EFI subdirectory from versioned path"
										);
										Self::copy_dir(&efi_subsrc, &efi_dest_subdir)?;
									} else {
										warn!(
											?efi_subsrc,
											"No EFI directory found in subdirectory of /usr/lib/efi"
										);
									}
								}
							}
						}
					}
				} else {
					bail!("No /usr/lib/efi directory found");
				}
			} else {
				warn!("No EFI directory found in {}", chroot_boot.display());
			}
			Ok(())
		})?;

		fs::copy(boot_imgs_dir.join("boot").join(&vmlinuz), iso_boot.join(&vmlinuz))?;
		fs::copy(boot_imgs_dir.join("boot").join(&initramfs), iso_boot.join("initramfs.img"))?;

		Ok((vmlinuz, "initramfs.img".to_string()))
	}

	fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
		if !src.exists() {
			bail!("Source directory {} does not exist", src.display());
		}
		if dest.exists() {
			// Actually just append files instead of removing existing directory
			// fs::remove_dir_all(dest)?;
		}
		fs::create_dir_all(dest)?;

		for entry in fs::read_dir(src)? {
			let entry = entry?;
			let entry_path = entry.path();
			let dest_path = dest.join(entry.file_name());
			let file_type = fs::symlink_metadata(&entry_path)?.file_type();
			if file_type.is_dir() {
				Self::copy_dir(&entry_path, &dest_path)?;
			} else if file_type.is_file() {
				fs::copy(&entry_path, &dest_path)?;
			} else if file_type.is_symlink() {
				let target = fs::read_link(&entry_path)?;
				symlink(target, &dest_path)?;
			}
		}

		Ok(())
	}

	fn generate_grub_config(
		&self, iso_tree: &Path, volid: String, distro: &str, vmlinuz: &str, initramfs: &str,
		kernel_cmdline: &str,
	) -> Result<()> {
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
		let arch_short = self.get_arch_short(manifest);
		let arch_short_upper = arch_short.to_uppercase();
		let arch_32 = self.get_arch_32bit(manifest).to_uppercase();

		fs::create_dir_all(iso_tree.join("EFI/BOOT/fonts"))?;

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

	fn mkefiboot(&self, workspace: &Path, _: &Manifest) -> Result<()> {
		let tree = workspace.join(ISO_TREE);

		let sparse_path = &tree.join("boot/efiboot.img");
		crate::util::create_sparse(sparse_path, 25 * 1024 * 1024)?;

		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		cmd_lib::run_cmd!(
			mkfs.msdos $ldp -v -n EFI 2>&1;
			mkdir -p /tmp/katsu.efiboot;
			mount $ldp /tmp/katsu.efiboot;
			mkdir -p /tmp/katsu.efiboot/EFI/BOOT;
			cp -avr $tree/EFI/BOOT/. /tmp/katsu.efiboot/EFI/BOOT 2>&1;
			umount /tmp/katsu.efiboot;
		)?;

		drop(hdl);
		Ok(())
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
		{
			use std::process::Command;

			let grub_mkimage_status = Command::new("grub2-mkimage")
				.arg("-O")
				.arg(arch_out)
				.arg("-d")
				.arg(chroot.join(format!("usr/lib/grub/{}", arch)))
				.arg("-o")
				.arg(iso_tree.join("boot/eltorito.img"))
				.arg("-p")
				.arg("/boot/grub")
				.arg("iso9660")
				.args(&arch_modules)
				.status()?;

			if !grub_mkimage_status.success() {
				bail!("grub2-mkimage command failed with status: {:?}", grub_mkimage_status);
			}

			let grub_mkrescue_status = Command::new("grub2-mkrescue")
				.arg("-o")
				.arg(iso_tree.join("../efiboot.img"))
				.status()?;

			if !grub_mkrescue_status.success() {
				bail!("grub2-mkrescue command failed with status: {:?}", grub_mkrescue_status);
			}
		}

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
}
