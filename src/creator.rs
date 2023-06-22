use color_eyre::{eyre::eyre, Result};
use std::path::Path;
use tracing::{debug, error, instrument, trace, warn};

use crate::{
	cfg::Config,
	donburi::{dracut, grub_mkconfig},
	run,
};

const ISO_L3_MAX_FILE_SIZE: u64 = 4 * 1024_u64.pow(3);

pub trait LiveImageCreator {
	/// src, dest, required
	const EFI_FILES: &'static [(&'static str, &'static str, bool)];
	const ARCH: crate::util::Arch;

	fn get_cfg(&self) -> &Config;

	fn copy_efi_files(&self, isodir: &str) -> Result<bool> {
		let mut fail = false;
		std::fs::create_dir_all(Path::new(isodir).join("EFI/BOOT/fonts"))?;
		for (src, dest, req) in Self::EFI_FILES {
			let src = src.replace("%arch%", Self::ARCH.into());
			let dest = dest.replace("%arch%", Self::ARCH.into());
			let root =
				&self.get_cfg().instroot.canonicalize().expect("Cannot canonicalize instroot.");
			let root = root.to_str().unwrap();
			let p = format!("{root}{src}");
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
		self.mkmountpt()?;
		self.initsys()?;
		self.postinit_script()?;
		self.instpkgs()?;
		let cfg = self.get_cfg();
		dracut(cfg)?;
		self.copy_efi_files(&cfg.isodir)?;
		self.squashfs()?;
		grub_mkconfig(&cfg.isodir)?;
		self.postinst_script()?;
		self.create_iso()?;
		Ok(())
	}

	fn postinit_script(&self) -> Result<()> {
		let cfg = self.get_cfg();
		if let Some(script) = &cfg.script.postinit {
			let root = &cfg.instroot.canonicalize()?;
			let rootname = root.to_str().unwrap();
			let name = script
				.file_name()
				.ok_or(eyre!("postinst script is not a file"))?
				.to_str()
				.ok_or(eyre!("Cannot get postinst filename in &str"))?;
			let dest = Path::join(root, name);
			std::fs::copy(script, &dest)?;
			run!(~"systemd-nspawn", "-D", &rootname, &format!("/{name}"))
				.map_err(|e| e.wrap_err("postinit script failed"))?;
			std::fs::remove_file(dest)?;
		}
		Ok(())
	}

	fn postinst_script(&self) -> Result<()> {
		let cfg = self.get_cfg();
		if let Some(script) = &cfg.script.postinst {
			let root = &cfg.instroot.canonicalize()?;
			let rootname = root.to_str().unwrap();
			let name = script
				.file_name()
				.ok_or(eyre!("postinst script is not a file"))?
				.to_str()
				.ok_or(eyre!("Cannot get postinst filename in &str"))?;
			let dest = Path::join(root, name);
			std::fs::copy(script, &dest)?;
			run!(~"systemd-nspawn", "-D", &rootname, &format!("/{name}"))
				.map_err(|e| e.wrap_err("postinst script failed"))?;
			std::fs::remove_file(dest)?;
		}
		Ok(())
	}

	fn squashfs(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let os_image = Path::new(&cfg.isodir)
			.join("LiveOS")
			.join("squashfs.img")
			.to_string_lossy()
			.to_string();

		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();

		run!(~"mksquashfs", root, &*os_image, "-comp", "gzip")?;
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

	fn _get_xorrisofs_options<'a>(&'a self) -> Vec<&'a str> {
		let cfg = self.get_cfg();
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
				if Path::new(cfg.isodir.as_str()).join(d).join(i0).exists() {
					let s: &'static String = Box::leak(Box::new(format!("{d}/{i0}")));
					let ss: &'static String = Box::leak(Box::new(format!("-isohybrid-gpt-{i1}")));
					options.append(&mut vec!["-eltorito-alt-boot", "-e", &s, "-no-emul-boot", &ss]);
					dirs = vec![d];
					break;
				}
			}
		}
		[options, vec!["-rational-rock", "-joliet", "-volid", &cfg.fs.label]].concat()
	}

	fn create_iso(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let mut args = vec![];
		if self._is_iso_level_3(&cfg.isodir)? {
			args.append(&mut vec!["-iso-level", "3"]);
		}
		args.append(&mut vec!["-output", &cfg.out, "-no-emul-boot"]);
		args.append(&mut self._get_xorrisofs_options());
		args.push(&cfg.isodir);
		if let Err(e) = run!(~"xorrisofs"; args) {
			error!("ISO creation failed!");
			return Err(e.wrap_err("Fail to create ISO using `xorrisofs`"));
		}
		self._implant_md5sum(&cfg.out)?;
		Ok(())
	}
	fn _implant_md5sum(&self, out: &str) -> Result<()> {
		for c in ["implantisomd5", "/usr/lib/anaconda-runtime/implantisomd5"] {
			if let Err(e) = run!(c, out) {
				if let Some(scode) = e.to_string().strip_prefix("Command returned code: ") {
					if scode.parse::<i16>().map_err(|e| {
						eyre!(e).wrap_err("Cannot parse implantisomd5 error: code not i16?")
					})? == 2
					{
						warn!("Faced ENOENT from `{c}`");
						// ENOENT?
						continue;
					}
				}
				return Err(e.wrap_err("Cannot implant md5sum"));
			} else {
				return Ok(());
			}
		}
		warn!("isomd5sum not installed; not setting up mediacheck");
		Ok(())
	}

	#[inline]
	fn _rel(&self) -> String {
		self.get_cfg().sys.releasever.to_string()
	}

	#[instrument(skip(self))]
	fn mkmountpt(&self) -> Result<()> {
		tracing::debug!("Checking for mount point");
		let cfg = self.get_cfg();
		if cfg.fs.skip.unwrap_or_default() {
			return Ok(());
		}
		let instroot = Path::new(&cfg.instroot);
		trace!("Checking for {instroot:?}");
		if instroot.is_dir() {
			debug!("Using preexisting dir as instroot: {instroot:?}");
			if let Some(Ok(_)) = std::fs::read_dir(instroot)?.next() {
				// return Err(eyre!("{instroot:?} is not empty."));
				warn!("{instroot:?} is not empty.");
			}
		} else {
			if instroot.is_file() {
				return Err(eyre!("Cannot make new fs on {instroot:?} because it's a file."));
			}
			std::fs::create_dir(instroot)?;
		}
		Ok(())
	}

	/// Initialise a system on `instroot`.
	#[instrument(skip(self))]
	fn initsys(&self) -> Result<()> {
		tracing::info!("Initializing system using DNF");
		let cfg = self.get_cfg();
		let rel = self._rel();
		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		run!(~"dnf", "-y", "--releasever", &rel, "--installroot", root, "in", "@core", "fedora-repos", "kernel")?;
		Ok(())
	}
	#[instrument(skip(self))]
	fn instpkgs(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let rel = self._rel();
		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let mut args = vec!["in", "-y", "--releasever", &rel, "--installroot", root];
		args.extend(cfg.packages.iter().map(|a| a.as_str()));
		run!(~"dnf"; args)?;
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

	#[inline]
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

	fn get_cfg(&self) -> &Config {
		&self.cfg
	}
}
