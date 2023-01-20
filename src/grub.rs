use color_eyre::Result;
use std::process::Command;
use tracing::error;

use crate::{cfg::Config, run};

/// Assume: `target` ends with `/`
pub fn grub_mkconfig(target: &str) -> Result<()> {
	run!("grub2-mkconfig", "-o", &format!("{target}boot/grub2/grub.cfg"))?;
	Ok(())
}
pub fn grub_install(disk: &str, arch: &str) -> Result<()> {
	let stat = Command::new("grub2-install").args([disk, "--target", arch]).status();
	if stat.is_ok() {
		Ok(())
	} else {
		Err(stat.err().unwrap().into())
	}
}

pub trait LiveImageCreator {
	/// src, dest, required
	const EFI_FILES: &'static [(&'static str, &'static str, bool)];
	const ARCH: crate::util::Arch;

	fn get_cfg(&self) -> &Config;

	fn copy_efi_files(&self, isodir: &str) -> Result<bool> {
		let mut fail = false;
		std::fs::create_dir_all(std::path::Path::new(isodir).join("EFI/BOOT/fonts"))?;
		for (src, dest, req) in Self::EFI_FILES {
			let p = format!("{}{src}", self.get_cfg().instroot);
			let p = std::path::Path::new(&p);
			if !p.exists() && *req {
				error!(src, "Missing EFI File");
				fail = true;
			} else {
				std::fs::copy(p, format!("{isodir}{dest}"))?;
			}
		}
		Ok(fail)
	}

	fn exec(&self) -> Result<()> {
		Ok(())
	}
}

pub struct LiveImageCreatorX86 {
	cfg: Config,
}

impl From<Config> for LiveImageCreatorX86 {
	fn from(cfg: Config) -> Self {
		Self { cfg }
	}
}

impl LiveImageCreator for LiveImageCreatorX86 {
	const ARCH: crate::util::Arch = crate::util::Arch::X86;
	const EFI_FILES: &'static [(&'static str, &'static str, bool)] = &[
		("boot/efi/EFI/*/gcd%arch%.efi", "/EFI/BOOT/grub%arc%.efi", true),
		("boot/efi/EFI/*/shimia32.efi", "/EFI/BOOT/BOOTIA32.EFI", false),
		("boot/efi/EFI/*/gcdia32.efi", "/EFI/BOOT/grubia32.efi", false),
		("usr/share/grub/unicode.pf2", "/EFI/BOOT/fonts/", true),
	];

	fn get_cfg(&self) -> &Config {
		&self.cfg
	}
}
pub struct LiveImageCreatorX86_64 {
	cfg: Config,
}

impl From<Config> for LiveImageCreatorX86_64 {
	fn from(cfg: Config) -> Self {
		Self { cfg }
	}
}

impl LiveImageCreator for LiveImageCreatorX86_64 {
	const ARCH: crate::util::Arch = crate::util::Arch::X86_64;
	const EFI_FILES: &'static [(&'static str, &'static str, bool)] = &[
		("boot/efi/EFI/*/gcd%arch%.efi", "/EFI/BOOT/grub%arc%.efi", true),
		("boot/efi/EFI/*/shimia32.efi", "/EFI/BOOT/BOOTIA32.EFI", false),
		("boot/efi/EFI/*/gcdia32.efi", "/EFI/BOOT/grubia32.efi", false),
		("usr/share/grub/unicode.pf2", "/EFI/BOOT/fonts/", true),
	];

	fn get_cfg(&self) -> &Config {
		&self.cfg
	}
}
