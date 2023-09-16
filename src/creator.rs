use color_eyre::{eyre::eyre, Help, Result};
use std::{io::Write, path::Path};
use tracing::{debug, error, info, instrument, trace, warn};

use crate::{cfg::Config, run};

const DEFAULT_DNF: &str = "dnf5";

pub trait LiveImageCreator {
	/// src, dest, required
	const EFI_FILES: &'static [(&'static str, &'static str, bool)];
	const ARCH: crate::util::Arch;

	fn get_cfg(&self) -> &Config;

	fn get_krnl_ver(target: &str) -> Result<String> {
		Ok(cmd_lib::run_fun!(rpm -q kernel --root $target)?)
	} 

	fn fstab(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let mut f = std::fs::File::create(format!("{}/etc/fstab", root.display()))?;
		f.write(b"/squashfs.img\t/\tsquashfs\tdefaults\t0\t0")?;
		Ok(())
	}

	fn dracut(&self) -> Result<()> {
		self.fstab()?;
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let kver = &Self::get_krnl_ver(root)?;
		let kver = kver.trim_start_matches("kernel-");
		// -I /.profile
		crate::run!(
			"dracut",
			"-r",
			root,
			"-vfNa",
			" pollcdrom dmsquash-live livenet network ",
			"--no-hostonly-cmdline",
			"-i",
			"fstab",
			&*format!("{root}/etc/fstab"),
			"--add-drivers",
			"overlay,squashfs",
			"-o",
			" multipath ",
			&*format!("{root}/boot/initramfs-{kver}.img"),
			kver
		)?;
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
		self.postinst_script()?;
		self.squashfs()?;
		self.liveos()?;
		self.xorriso()?;
		self.bootloader()?;
		info!("Done: {}.iso", cfg.out);
		Ok(())
	}

	fn bootloader(&self) -> Result<()> {
		info!("Installing Limine bootloader");
		let out = &self.get_cfg().out;
		run!("limine", "bios-install", &*format!("{out}.iso"))?;
		Ok(())
	}

	/// Returns volid
	fn liveos(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let distro = &cfg.distro;
		let out = &cfg.out;
		std::fs::create_dir_all(format!("./{distro}/LiveOS"))?;
		std::fs::copy(
			"/usr/share/limine/limine-uefi-cd.bin",
			format!("./{distro}/LiveOS/boot/limine-uefi-cd.bin"),
		)?;
		std::fs::copy(
			"/usr/share/limine/limine-bios-cd.bin",
			format!("./{distro}/LiveOS/boot/limine-bios-cd.bin"),
		)?;
		std::fs::copy(
			"/usr/share/limine/limine-bios.sys",
			format!("./{distro}/LiveOS/boot/limine-bios.sys"),
		)?;
		self.limine_cfg(&*format!("./{distro}/LiveOS/boot/limine.cfg"), distro)?;

		std::fs::rename(format!("{out}.img"), format!("./{distro}/LiveOS/squashfs.img"))?;
		Ok(())
	}

	/// Returns volid
	fn limine_cfg(&self, path: &str, distro: &str) -> Result<()> {
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let kver = &Self::get_krnl_ver(root.to_str().unwrap())?;
		let kver = kver.trim_start_matches("kernel-");
		let volid = &cfg.volid;
		let mut f = std::fs::File::create(path)
			.map_err(|e| eyre!(e).wrap_err("Cannot create limine.cfg"))?;

		f.write_fmt(format_args!("TIMEOUT=5\n\n:{distro}\n\tPROTOCOL=linux\n\t"))?;
		f.write_fmt(format_args!("KERNEL_PATH=boot:///boot/vmlinuz-{kver}\n\t"))?;
		f.write_fmt(format_args!("MODULE_PATH=boot:///boot/initramfs-{kver}.img\n\t"))?;
		f.write_fmt(format_args!("CMDLINE=root=live:LABEL={volid} rd.live.image"))?;
		Ok(())
	}

	fn xorriso(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let distro = &cfg.distro;
		let out = &cfg.out;
		let volid = &cfg.volid;
		run!(
			"xorriso",
			"-as",
			"mkisofs",
			"-b",
			&format!("boot/limine-bios-cd.bin"),
			"-no-emul-boot",
			"-boot-load-size",
			"4",
			"-boot-info-table",
			"--efi-boot",
			&format!("boot/limine-uefi-cd.bin"),
			"-efi-boot-part",
			"--efi-boot-image",
			"--protective-msdos-label",
			Path::new(&format!("./{distro}/LiveOS/")).canonicalize()?.to_str().unwrap(),
			"-volid",
			volid,
			"-o",
			&format!("./{out}.iso"),
		)?;
		Ok(())
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
		let instroot = &cfg.instroot;
		let distro = &cfg.distro;

		cmd_lib::run_cmd!(
			mkdir -p $distro/LiveOS/boot;
			sh -c "cp $instroot/boot/vmlinuz-* $instroot/boot/initramfs-* $distro/LiveOS/boot/";
		)?;

		info!("Squashing fs");

		run!(~"mksquashfs", root, &name, "-comp", "gzip", "-noappend")?;
		Ok(())
	}

	fn erofs(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let name = format!("{}.efs.img", cfg.out);
		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();

		info!("Squashing fs");

		run!(~"mkfs.erofs", &name, root, "-zlz4hc")?;
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
