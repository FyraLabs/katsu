use color_eyre::{eyre::eyre, Help, Result};
use std::path::Path;
use tracing::{debug, error, info, instrument, trace, warn};

use crate::{cfg::Config, run};

const ISO_L3_MAX_FILE_SIZE: u64 = 4 * 1024_u64.pow(3);
const DEFAULT_DNF: &str = "dnf5";

pub trait LiveImageCreator {
	/// src, dest, required
	const EFI_FILES: &'static [(&'static str, &'static str, bool)];
	const ARCH: crate::util::Arch;

	fn get_cfg(&self) -> &Config;

	fn get_krnl_ver(target: &str) -> Result<String> {
		Ok(cmd_lib::run_fun!(rpm -q kernel --root $target)?)
	}

	fn dracut(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let kver = &Self::get_krnl_ver(root)?;
		// -I /.profile
		cmd_lib::run_cmd!(dracut -r $root -vfNa " kiwi-live pollcdrom " --no-hostonly-cmdline -o " multipath " $root/boot/initramfs-$kver.img $kver)?;
		Ok(())
	}

	fn copy_efi_files(&self, instroot: &Path) -> Result<bool> {
		info!("Copying EFI files");
		let mut fail = false;
		std::fs::create_dir_all(Path::new(instroot).join("EFI/BOOT/fonts"))?;
		for (srcs, dest, req) in Self::EFI_FILES {
			let srcs = srcs.replace("%arch%", Self::ARCH.into());
			let dest = dest.replace("%arch%", Self::ARCH.into());
			let root =
				&self.get_cfg().instroot.canonicalize().expect("Cannot canonicalize instroot.");
			let root = root.to_str().unwrap();
			for src in glob::glob(&srcs).expect("Failed to read glob pattern") {
				let src = src?;
				let p = Path::new(&root).join(&src);
				if p.exists() {
					let realdest = Path::new(instroot).join(&dest);
					std::fs::create_dir_all(&realdest).map_err(|e| {
						eyre!(e)
							.wrap_err("Cannot create EFI destination")
							.note(realdest.display().to_string())
					})?;
					let fname = p
						.file_name()
						.ok_or_else(|| eyre!("Cannot get file name for `{p:?}`"))?
						.to_str()
						.ok_or_else(|| eyre!("Cannot convert file name for `{p:?}`"))?;
					let realdest = format!("{}{fname}", realdest.display());
					std::fs::copy(&p, &realdest).map_err(|e| {
						eyre!(e)
							.wrap_err("Cannot copy EFI files")
							.note(format!("Destination: {realdest}"))
							.note(format!("Source: {p:?}"))
					})?;
				} else if *req {
					error!(?src, "Missing EFI File");
					fail = true;
				}
			}
		}
		Ok(fail)
	}

	fn exec(&self) -> Result<()> {
		self.mkmountpt()?;
		self.init_script()?;
		self.instpkgs()?;
		self.dracut()?;
		let cfg = self.get_cfg();
		self.copy_efi_files(&cfg.instroot)?;
		self.grub_mkconfig(cfg)?;
		self.postinst_script()?;
		self.squashfs()?;
		self.create_iso()?;
		Ok(())
	}

	fn grub_mkconfig(&self, cfg: &Config) -> Result<()> {
		todo!();
		// let target = cfg
		// 	.instroot
		// 	.canonicalize()
		// 	.expect("Cannot canonocalize instroot")
		// 	.display()
		// 	.to_string();
		// run!("systemd-nspawn", "-D", &target, "grub2-mkconfig", "-o", "/boot/grub2/grub.cfg")?;
		// Ok(())
	}

	fn init_script(&self) -> Result<()> {
		let cfg = self.get_cfg();
		if let Some(script) = &cfg.script.init {
			let root = &cfg.instroot.canonicalize()?;
			let rootname = root.to_str().unwrap();
			let name = script
				.file_name()
				.ok_or(eyre!("init script is not a file"))?
				.to_str()
				.ok_or(eyre!("Cannot get init filename in &str"))?;
			let dest = Path::join(root, name);
			debug!(?script, ?dest, "Copying init script");
			std::fs::copy(script, &dest)?;
			info!(?script, "Running init script");
			cmd_lib::run_cmd! (
				cd $rootname;
				sh $name
			)
			.map_err(|e| eyre!(e).wrap_err("init script failed"))?;
			debug!(?dest, "Removing init script");
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
			debug!(?script, ?dest, "Copying postinst script");
			std::fs::copy(script, &dest)?;
			info!(?script, "Running postinst script");
			run!(~"systemd-nspawn", "-D", &rootname, &format!("/{name}"))
				.map_err(|e| e.wrap_err("postinst script failed"))?;
			debug!(?dest, "Removing postinst script");
			std::fs::remove_file(dest)?;
		}
		Ok(())
	}

	fn squashfs(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let name = format!("{}.img", cfg.out);
		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();

		info!("Squashing fs");

		run!(~"mksquashfs", root, &name, "-comp", "gzip", "-noappend")?;
		Ok(())
	}

	fn _is_iso_level_3<P: AsRef<Path>>(&self, dir: P) -> Result<bool> {
		for entry in std::fs::read_dir(dir)? {
			let entry = entry?;
			if entry.file_type()?.is_dir() {
				if self._is_iso_level_3(entry.path())? {
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
				if Path::new(&cfg.instroot.display().to_string()).join(d).join(i0).exists() {
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
		if self._is_iso_level_3(
			&cfg.instroot
				.canonicalize()
				.map_err(|e| eyre!(e).wrap_err("Cannot canonicalize instroot"))?,
		)? {
			args.append(&mut vec!["-iso-level", "3"]);
		}
		let out = format!("{}.iso", cfg.out);
		args.append(&mut vec!["-output", &out, "-no-emul-boot"]);
		args.append(&mut self._get_xorrisofs_options());
		let binding = cfg.instroot.display().to_string();
		args.push(&binding);
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
		debug!("Checking for mount point");
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

	#[instrument(skip(self))]
	fn instpkgs(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let dnf = cfg.dnf.as_ref().map_or(DEFAULT_DNF, |x| &x);
		info!(dnf, "Installing packages");
		let rel = self._rel();
		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let pkgs: Vec<&str> = cfg.packages.iter().map(|x| x.as_str()).collect();
		cmd_lib::run_cmd!($dnf in -y --releasever $rel --installroot $root --use-host-config $[pkgs])?;
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
