use super::{Bootloader, LIMINE_PREPEND_COMMENT};
use crate::{builder::ISO_TREE, config::Manifest};
use color_eyre::Result;
use std::path::Path;
use tracing::info;

impl Bootloader {
	pub(super) fn cp_limine(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		info!("Copying Limine files");
		let distro = manifest.distro.as_deref().unwrap_or("Linux");
		let cmd = manifest.kernel_cmdline.as_deref().unwrap_or("");
		let root = chroot.parent().unwrap().join(ISO_TREE);

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

		let limine_cfg = root.join("boot/limine.cfg");
		crate::tpl!(
			"limine.cfg.tera" => { LIMINE_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &limine_cfg
		);

		let binding = cmd_lib::run_fun!(b2sum $limine_cfg)?;
		let liminecfg_b2h = binding.split_whitespace().next().unwrap();

		tracing::info_span!("Enrolling Limine Secure Boot").in_scope(|| -> Result<()> {
			Ok(cmd_lib::run_cmd!(
				limine enroll-config $root/boot/limine-uefi-cd.bin $liminecfg_b2h 2>&1;
				limine enroll-config $root/boot/limine-bios.sys $liminecfg_b2h 2>&1;
			)?)
		})?;

		Ok(())
	}
}
