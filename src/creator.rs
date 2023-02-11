use color_eyre::Result;
use smartstring::alias::String as SStr;
use std::{path::Path, process::Command};
use tracing::{error, info, instrument, warn};

use crate::{cfg::Config, run};

const ISO_L3_MAX_FILE_SIZE: u64 = 4 * 1024_u64.pow(3);

/// Assume: `target` ends with `/`
pub fn grub_mkconfig(target: &str) -> Result<()> {
	run!("grub2-mkconfig", "-o", &format!("{target}boot/grub2/grub.cfg"))?;
	Ok(())
}

pub trait LiveImageCreator {
	/// src, dest, required
	const EFI_FILES: &'static [(&'static str, &'static str, bool)];
	const ARCH: crate::util::Arch;

	fn get_cfg(&self) -> &Config;

	fn copy_efi_files(&self, isodir: &str) -> Result<bool> {
		let mut fail = false;
		std::fs::create_dir_all(Path::new(isodir).join("EFI/BOOT/fonts"))?;
		for (src, dest, req) in Self::EFI_FILES {
			let src = src.replace("%arch", Self::ARCH.into());
			let dest = dest.replace("%arch", Self::ARCH.into());
			let p = format!("{}{src}", self.get_cfg().instroot);
			let p = Path::new(&p);
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

	fn _is_iso_level_3<P: AsRef<Path>>(&self, dir: P) -> Result<bool> {
		for entry in std::fs::read_dir(dir)? {
			let entry = entry?;
			if entry.file_type()?.is_dir() {
				if self._is_iso_level_3(entry.file_name())? {
					return Ok(true);
				}
			} else {
				if entry.metadata()?.len() >= ISO_L3_MAX_FILE_SIZE {
					return Ok(true);
				}
			}
		}
		Ok(false)
	}

	fn _get_xorrisofs_options<'a>(&'a self) -> Vec<&'a str>;

	fn create_iso(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let mut args = vec![];
		if self._is_iso_level_3(&cfg.isodir)? {
			args.append(&mut vec!["-iso-level", "3"]);
		}
		args.append(&mut vec!["-output", &cfg.out, "-no-emul-boot"]);
		args.append(&mut self._get_xorrisofs_options());
		args.push(&cfg.isodir);
		if let Err(e) = run!("xorrisofs"; args) {
			error!("ISO creation failed!");
			return Err(e);
		}
		self._implant_md5sum(&cfg.out)?;
		Ok(())
	}
	fn _implant_md5sum(&self, out: &str) -> Result<()> {
		for c in ["implantisomd5", "/usr/lib/anaconda-runtime/implantisomd5"] {
			if let Err(e) = run!(c, out) {
				if let Some(scode) = e.to_string().strip_prefix("Command returned code: ") {
					if scode.parse::<i16>().expect("code not i16?") == 2 {
						// ENOENT?
						continue;
					}
				}
				return Err(e);
			} else {
				return Ok(());
			}
		}
		warn!("isomd5sum not installed; not setting up mediacheck");
		Ok(())
	}

	#[inline]
	fn _rel(&self) -> String {
		format!("{}", self.get_cfg().sys.releasever)
	}

	/// Initialise a system on `instroot`.
	/// This also installs the groups listed in cfg.
	#[instrument(skip(self))]
	fn initsys(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let rel = self._rel();
		let mut args =
			vec!["--releasever", &rel, "--installroot", &cfg.instroot, "groupinstall", "core"];
		args.extend(cfg.packages.grps.iter().map(|a| a.as_str()));
		run!("dnf"; args)?;
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
		("/boot/efi/EFI/*/shim%arch%.efi", "/EFI/BOOT/BOOT%arch%.EFI", true),
		("/boot/efi/EFI/*/gcd%arch%.efi", "/EFI/BOOT/grub%arch%.efi", true),
		("/boot/efi/EFI/*/shimia32.efi", "/EFI/BOOT/BOOTIA32.EFI", false),
		("/boot/efi/EFI/*/gcdia32.efi", "/EFI/BOOT/grubia32.efi", false),
		("/usr/share/grub/unicode.pf2", "/EFI/BOOT/fonts/", true),
	];

	fn _get_xorrisofs_options<'a>(&'a self) -> Vec<&'a str> {
		let mut options = vec![
			"-eltorito-boot",
			"isolinux/isolinux.bin",
			"-no-emul-boot",
			"-boot-info-table",
			"-boot-load-size",
			"4",
			"-eltorito-catalog",
			"isolinux/boot.cat",
			"-isohybrid-mbr",
			"/usr/share/syslinux/isohdpfx.bin",
		];
		let mut dirs = vec!["images", "isolinux"];
		for (i0, i1) in [("efiboot.img", "basdat"), ("macboot.img", "hfsplus")] {
			for d in &dirs {
				if Path::new(self.cfg.isodir.as_str()).join(d).join(i0).exists() {
					let s: &'static String = Box::leak(Box::new(format!("{d}/{i0}")));
					let ss: &'static String = Box::leak(Box::new(format!("-isohybrid-gpt-{i1}")));
					options.append(&mut vec![
						"-eltorito-alt-boot",
						"-e",
						&s,
						"-no-emul-boot",
						&ss,
					]);
					dirs = vec![d];
					break;
				}
			}
		}
		options.append(&mut vec!["-rational-rock", "-joliet", "-volid", &self.cfg.fslabel]);
		options
	}


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
		("/boot/efi/EFI/*/shim%arch%.efi", "/EFI/BOOT/BOOT%arch%.EFI", true),
		("/boot/efi/EFI/*/gcd%arch%.efi", "/EFI/BOOT/grub%arch%.efi", true),
		("/boot/efi/EFI/*/shimia32.efi", "/EFI/BOOT/BOOTIA32.EFI", false),
		("/boot/efi/EFI/*/gcdia32.efi", "/EFI/BOOT/grubia32.efi", false),
		("/usr/share/grub/unicode.pf2", "/EFI/BOOT/fonts/", true),
	];

	fn _get_xorrisofs_options<'a>(&'a self) -> Vec<&'a str> {
		let mut options = vec![
			"-eltorito-boot",
			"isolinux/isolinux.bin",
			"-no-emul-boot",
			"-boot-info-table",
			"-boot-load-size",
			"4",
			"-eltorito-catalog",
			"isolinux/boot.cat",
			"-isohybrid-mbr",
			"/usr/share/syslinux/isohdpfx.bin",
		];
		let mut dirs = vec!["images", "isolinux"];
		for (i0, i1) in [("efiboot.img", "basdat"), ("macboot.img", "hfsplus")] {
			for d in &dirs {
				if Path::new(self.cfg.isodir.as_str()).join(d).join(i0).exists() {
					let s: &'static String = Box::leak(Box::new(format!("{d}/{i0}")));
					let ss: &'static String = Box::leak(Box::new(format!("-isohybrid-gpt-{i1}")));
					options.append(&mut vec![
						"-eltorito-alt-boot",
						"-e",
						&s,
						"-no-emul-boot",
						&ss,
					]);
					dirs = vec![d];
					break;
				}
			}
		}
		options.append(&mut vec!["-rational-rock", "-joliet", "-volid", &self.cfg.fslabel]);
		options
	}

	fn get_cfg(&self) -> &Config {
		&self.cfg
	}
}
