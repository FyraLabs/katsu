use color_eyre::Result;
use std::path::Path;
use sys_mount::Unmount;
use tracing::{debug, info, trace, warn};

use crate::{bail_let, cmd};

use super::manifest::Manifest;

crate::prepend_comment!(GRUB_PREPEND_COMMENT: "/boot/grub/grub.cfg", "Grub configurations", katsu::builder::Bootloader::cp_grub);
crate::prepend_comment!(LIMINE_PREPEND_COMMENT: "/boot/limine.cfg", "Limine configurations", katsu::builder::Bootloader::cp_limine);
const ISO_TREE: &str = "iso-tree";

#[derive(Default, Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bootloader {
	#[default]
	#[serde(alias = "grub2")]
	Grub,
	Limine,
	SystemdBoot,
}

impl From<&str> for Bootloader {
	fn from(value: &str) -> Self {
		match &*value.to_lowercase() {
			"limine" => Self::Limine,
			"grub" | "grub2" => Self::Grub,
			"systemd-boot" => Self::SystemdBoot,
			_ => {
				warn!("Unknown bootloader: {value}, falling back to GRUB");
				Self::Grub
			},
		}
	}
}

impl Bootloader {
	pub fn install(&self, image: &Path) -> Result<()> {
		match *self {
			Self::Grub => info!("GRUB is not required to be installed to image, skipping"),
			Self::Limine => cmd!(? "limine" "bios-install" {{ image.display() }})?,
			Self::SystemdBoot => cmd!(? "bootctl" ["--image={}" image.display()] "install")?,
		}
		Ok(())
	}
	#[must_use]
	pub fn get_bins(&self) -> (&'static str, &'static str) {
		match *self {
			Self::Grub => ("boot/efi/EFI/fedora/shim.efi", "boot/eltorito.img"),
			Self::Limine => ("boot/limine-uefi-cd.bin", "boot/limine-bios-cd.bin"),
			Self::SystemdBoot => todo!(),
		}
	}
	fn cp_vmlinuz_initramfs(&self, chroot: &Path, dest: &Path) -> Result<(String, String)> {
		trace!("Finding vmlinuz and initramfs");
		let bootdir = chroot.join("boot");
		let mut vmlinuz = None;
		let mut initramfs = None;
		for f in bootdir.read_dir()? {
			let f = f?;
			if !f.metadata()?.is_file() {
				continue;
			}
			let name = f.file_name();
			debug!(?name, "File in /boot");
			let name = name.to_string_lossy();
			if name.contains("-rescue-") {
				continue;
			}

			if name.starts_with("vmlinuz-") {
				vmlinuz = Some(name.to_string());
			} else if name.starts_with("initramfs-") {
				initramfs = Some(name.to_string());
			}
			if vmlinuz.is_some() && initramfs.is_some() {
				break;
			}
		}

		bail_let!(Some(vmlinuz) = vmlinuz => "Cannot find vmlinuz in {bootdir:?}");
		bail_let!(Some(initramfs) = initramfs => "Cannot find initramfs in {bootdir:?}");

		trace!(vmlinuz, initramfs, "Copying vmlinuz and initramfs");
		std::fs::create_dir_all(dest.join("boot"))?;
		std::fs::copy(bootdir.join(&vmlinuz), dest.join("boot").join(&vmlinuz))?;
		std::fs::copy(bootdir.join(&initramfs), dest.join("boot").join(&initramfs))?;

		Ok((vmlinuz, initramfs))
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

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &root)?;
		let volid = manifest.get_volid();

		// Generate limine.cfg
		let limine_cfg = root.join("boot/limine.cfg");
		crate::tpl!("../../templates/limine.cfg.tera" => { LIMINE_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &limine_cfg);

		let binding = cmd!(stdout "b2sum" {{ limine_cfg.display() }});
		let liminecfg_b2h = binding.split_whitespace().next().unwrap();

		// enroll limine secure boot
		tracing::info_span!("Enrolling Limine Secure Boot").in_scope(|| -> Result<()> {
			cmd!(? "limine" "enroll-config" ["{root:?}/boot/limine-uefi-cd.bin"] liminecfg_b2h)?;
			cmd!(? "limine" "enroll-config" ["{root:?}/boot/limine-bios.sys"] liminecfg_b2h)?;
			Ok(())
		})?;

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
		let (ldp, hdl) = crate::util::loopdev_with_file(sparse_path)?;

		// Format disk with mkfs.fat
		cmd!(? "mkfs.msdos" {{ ldp.display() }} "-v" "-n" "EFI")?;
		// Mount disk to /tmp/katsu.efiboot
		std::fs::create_dir("/tmp/katsu.efiboot")?;
		let efimnt = sys_mount::Mount::new(&ldp, "/tmp/katsu.efiboot")?;
		std::fs::create_dir("/tmp/katsu.efiboot/EFI")?;
		std::fs::create_dir("/tmp/katsu.efiboot/EFI/BOOT")?;
		cmd!(? "cp" "-avr" ["{tree:?}/EFI/BOOT/."] "/tmp/katsu.efiboot/EFI/BOOT")?;
		efimnt.unmount(sys_mount::UnmountFlags::empty())?;

		drop(hdl);
		Ok(())
	}

	// todo: rewrite this whole thing, move ISO into a dedicated wrapper struct
	fn cp_grub(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		let imgd = chroot.parent().unwrap().join(ISO_TREE);
		let cmd = &manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let volid = manifest.get_volid();

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &imgd)?;

		let _ = std::fs::remove_dir_all(imgd.join("boot"));
		cmd!(? "cp" "-r" ["{chroot:?}/boot"] {{ imgd.display() }})?;
		std::fs::rename(imgd.join("boot/grub2"), imgd.join("boot/grub"))?;

		let distro = &manifest.distro.as_ref().map_or("Linux", |s| s);

		crate::tpl!("../../templates/grub.cfg.tera" => { GRUB_PREPEND_COMMENT, volid, distro, vmlinuz, initramfs, cmd } => imgd.join("boot/grub/grub.cfg"));

		let arch_short = match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH) {
			"x86_64" => "x64",
			"aarch64" => "aa64",
			_ => unimplemented!(),
		};

		let arch_short_upper = arch_short.to_uppercase();

		let arch_32 = match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH) {
			"x86_64" => "ia32",
			"aarch64" => "arm",
			_ => unimplemented!(),
		}
		.to_uppercase();

		// Funny script to install GRUB
		let _ = std::fs::create_dir_all(imgd.join("EFI/BOOT/fonts"));
		cmd!(? "cp" "-av" ["{imgd:?}/boot/efi/EFI/fedora/."] ["{imgd:?}/EFI/BOOT"])?;
		cmd!(? "cp" "-av" ["{imgd:?}/boot/grub/grub.cfg"] ["{imgd:?}/EFI/BOOT/BOOT.conf 2>&1"])?;
		cmd!(? "cp" "-av" ["{imgd:?}/boot/grub/grub.cfg"] ["{imgd:?}/EFI/BOOT/grub.cfg 2>&1"])?;
		cmd!(? "cp" "-av" ["{imgd:?}/boot/grub/fonts/unicode.pf2"] ["{imgd:?}/EFI/BOOT/fonts"])?;
		cmd!(? "cp" "-av" ["{imgd:?}/EFI/BOOT/shim${arch_short}.efi"] ["{imgd:?}/EFI/BOOT/BOOT${arch_short_upper}.efi"])?;
		cmd!(? "cp" "-av" ["{imgd:?}/EFI/BOOT/shim.efi"] ["{imgd:?}/EFI/BOOT/BOOT${arch_32}.efi"])?;

		// and then we need to generate eltorito.img
		let host_arch = std::env::consts::ARCH;

		let arch = match manifest.dnf.arch.as_deref().unwrap_or(host_arch) {
			"x86_64" => "i386-pc",
			"aarch64" => "arm64-efi",
			_ => unimplemented!(),
		};

		let arch_out = match manifest.dnf.arch.as_deref().unwrap_or(host_arch) {
			"x86_64" => "i386-pc-eltorito",
			"aarch64" => "arm64-efi",
			_ => unimplemented!(),
		};

		let arch_modules = match manifest.dnf.arch.as_deref().unwrap_or(host_arch) {
			"x86_64" => "biosdisk",
			"aarch64" => "efi_gop",
			_ => unimplemented!(),
		};

		debug!("Generating Grub images");
		// todo: uefi support
		cmd!(? "grub2-mkimage" "-O" arch_out "-d" ["{chroot:?}/usr/lib/grub/{arch}"] "-o" ["{imgd:?}/boot/eltorito.img"] "-p" "/boot/grub" "iso9660" arch_modules)?;
		// make it 2.88 MB
		// fallocate -l 1228800 $imgd/boot/eltorito.img;
		// ^ Commented out because it just wiped the entire file - @korewaChino
		// grub2-mkimage -O $arch_64-efi -d $chroot/usr/lib/grub/$arch_64-efi -o $imgd/boot/efiboot.img -p /boot/grub iso9660 efi_gop efi_uga 2>&1;
		cmd!(? "grub2-mkrescue" "-o" ["{imgd:?}/../efiboot.img"])?;

		debug!("Copying EFI files from Grub rescue image");
		let (ldp, hdl) = crate::util::loopdev_with_file(&imgd.join("../efiboot.img"))?;

		std::fs::create_dir("/tmp/katsu-efiboot")?;
		let mnt = sys_mount::Mount::new(ldp, "/tmp/katsu-efiboot")?;
		cmd!(? "cp" "-r" "/tmp/katsu-efiboot/boot/grub" ["{imgd:?}/boot/"])?;
		mnt.unmount(sys_mount::UnmountFlags::empty())?;

		drop(hdl);

		self.mkefiboot(chroot, manifest)?;

		Ok(())
	}

	pub fn copy_liveos(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		info!("Copying bootloader files");
		match *self {
			Self::Grub => self.cp_grub(manifest, chroot)?,
			Self::Limine => self.cp_limine(manifest, chroot)?,
			Self::SystemdBoot => todo!(),
		}
		Ok(())
	}
}
