use super::{Bootloader, REFIND_PREPEND_COMMENT};
use crate::{builder::ISO_TREE, config::Manifest, util::loopdev_with_file};
use color_eyre::Result;
use std::{fs, io::Write, path::Path};
use tracing::info;

impl Bootloader {
	pub(super) fn cp_refind(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		info!("Copying rEFInd files");
		let distro = manifest.distro.as_deref().unwrap_or("Linux");
		let cmd = manifest.kernel_cmdline.as_deref().unwrap_or("");
		let iso_tree = chroot.parent().unwrap().join(ISO_TREE);

		fs::create_dir_all(iso_tree.join("EFI/BOOT"))?;

		fs::copy("/usr/share/rEFInd/refind/refind_x64.efi", iso_tree.join("EFI/BOOT/BOOTX64.EFI"))?;

		fs::create_dir_all(iso_tree.join("EFI/BOOT/drivers_x64"))?;

		fs::copy(
			"/usr/share/rEFInd/refind/drivers_x64/iso9660_x64.efi",
			iso_tree.join("EFI/BOOT/drivers_x64/iso9660_x64.efi"),
		)?;

		fs::copy(
			"/usr/share/rEFInd/refind/drivers_x64/ext4_x64.efi",
			iso_tree.join("EFI/BOOT/drivers_x64/ext4_x64.efi"),
		)?;

		fs::create_dir_all(iso_tree.join("EFI/BOOT/icons"))?;

		cmd_lib::run_cmd!(
			cp -rv /usr/share/rEFInd/refind/icons/. $iso_tree/EFI/BOOT/icons/ 2>&1;
		)?;

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &iso_tree, false)?;
		let volid = manifest.get_volid();

		let refind_cfg = iso_tree.join("EFI/BOOT/refind.conf");
		crate::tpl!(
			"refind.cfg.tera" => { REFIND_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &refind_cfg
		);

		let mut nsh = fs::File::create(iso_tree.join("startup.nsh"))?;
		writeln!(nsh, "EFI\\BOOT\\BOOTX64.EFI")?;

		self.mk_refind_efiboot(chroot, manifest)?;

		Ok(())
	}

	fn mk_refind_efiboot(&self, chroot: &Path, _: &Manifest) -> Result<()> {
		let tree = chroot.parent().unwrap().join(ISO_TREE);

		let sparse_path = &tree.join("boot/efiboot.img");
		crate::util::create_sparse(sparse_path, 256 * 1024 * 1024)?;

		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		cmd_lib::run_cmd!(
			mkfs.msdos $ldp -v -n EFI 2>&1;
			mkdir -p /tmp/katsu.efiboot;
			mount $ldp /tmp/katsu.efiboot;
			mkdir -p /tmp/katsu.efiboot/EFI/BOOT;
			cp -avr $tree/EFI/BOOT/. /tmp/katsu.efiboot/EFI/BOOT 2>&1;
			mkdir -p /tmp/katsu.efiboot/boot;
			cp -av $tree/boot/vmlinuz /tmp/katsu.efiboot/boot/ 2>&1;
			cp -av $tree/boot/initramfs.img /tmp/katsu.efiboot/boot/ 2>&1;
			umount /tmp/katsu.efiboot;
		)?;

		drop(hdl);
		Ok(())
	}
}
