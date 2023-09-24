use color_eyre::{eyre::eyre, Help, Result};
use std::{
	fs,
	io::Write,
	path::{Path, PathBuf},
};
use tracing::{debug, error, info, instrument, trace, warn};
use tracing_subscriber::field::debug;

use crate::{
	cfg::{Config, OutputFormat},
	run,
	util::Arch,
};

const DEFAULT_DNF: &str = "dnf5";
const DEFAULT_BOOTLOADER: &str = "limine";
// const UBOOT_DATA: &str = "/usr/share/uboot";


pub trait ImageCreator {
	/// src, dest, required
	const EFI_FILES: &'static [(&'static str, &'static str, bool)];
	// const ARCH: crate::util::Arch;

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

	fn genfstab(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let out = format!("{}/etc/fstab", root);
		cmd_lib::run_cmd!(
			mkdir -p $root/etc;
		)?;
		// list mounts in $root
		let mounts = cmd_lib::run_fun!(findmnt -n -o UUID,TARGET,FSTYPE,OPTIONS --real --raw --noheadings --notruncate --output-all --target $root)?;

		// convert to fstab format
		let mut mounts = mounts
			.lines()
			.map(|x| {
				let mut x = x.split_whitespace();
				let uuid = x.next().unwrap();
				let target = x.next().unwrap();
				let fstype = x.next().unwrap();
				let options = x.next().unwrap();
				format!(
					"UUID={uuid}\t{target}\t{fstype}\t{options}\t0\t0",
					uuid = uuid,
					target = target,
					fstype = fstype,
					options = options
				)
			})
			.collect::<Vec<String>>()
			.join("\n");
		mounts.push('\n');

		debug!(?mounts, "Mounts");
		let mut f = std::fs::File::create(out)?;
		f.write_all(mounts.as_bytes())?;
		Ok(())
	}

	fn dracut(&self) -> Result<()> {
		// self.fstab()?;
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let kver = &Self::get_krnl_ver(root)?;
		let kver = kver.trim_start_matches("kernel-");
		info!(kver, root, "Generating initramfs");
		// -I /.profile
		crate::run!(~
			"dracut",
			"--xz",
			"-r",
			root,
			"-vfNa",
			"livenet dmsquash-live dmsquash-live-ntfs convertfs pollcdrom qemu qemu-net",
			"--omit",
			"plymouth",
			"--no-early-microcode",
			// "-i",
			// "fstab",
			// &*format!("{root}/etc/fstab"),
			// "--add-drivers",
			// "overlay,squashfs",
			"-o",
			" multipath ",
			&*format!("{root}/boot/initramfs-{kver}.img"),
			kver
		)?;
		Ok(())
	}

	fn copy_uboot_files(&self, bootpath: &Path) -> Result<()> {
		info!("Copying U-Boot files");
		// copy u-boot files to bootpath

		// dictionary of files to copy and destination

		let files = vec![
			("rpi_4/u-boot.bin", "rpi4-u-boot.bin"),
			("rpi_3/u-boot.bin", "rpi3-u-boot.bin"),
			("rpi_arm64/u-boot.bin", "rpi-u-boot.bin"),
		];

		// fs::copy(UBOOT_DATA, bootpath)?;

		Ok(())
	}

	fn get_arch(&self) -> Result<Arch> {
		let cfg = self.get_cfg();
		Ok(match cfg.arch.as_ref().is_some() {
			true => Arch::from(cfg.arch.as_ref().unwrap().as_str()),
			false => Arch::get()?,
		})
	}

	fn copy_efi_files(&self, instroot: &Path) -> Result<bool> {
		info!("Copying EFI files");
		// get config arch
		let arch_str: &str = self.get_arch()?.into();
		let mut fail = false;
		std::fs::create_dir_all(Path::new(instroot).join("EFI/BOOT/fonts"))?;
		for (srcs, dest, req) in Self::EFI_FILES {
			let srcs = srcs.replace("%arch%", arch_str);
			let dest = dest.replace("%arch%", arch_str);
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

	/// Redirects to output-specific functions
	fn exec(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let out_fmt = &cfg.format;

		match out_fmt {
			OutputFormat::Iso => self.exec_iso(),
			OutputFormat::Disk => self.exec_disk(),
		}
	}

	fn exec_iso(&self) -> Result<()> {
		self.mkmountpt()?;
		self.init_script()?;
		self.instpkgs()?;
		self.dracut()?;
		self.rootpw()?;
		self.postinst_script()?;
		self.squashfs()?;
		self.liveos()?;
		self.xorriso()?;
		self.bootloader()?;
		let cfg = self.get_cfg();
		info!("Done: {}.iso", cfg.out);
		Ok(())
	}

	fn exec_disk(&self) -> Result<()> {
		self.mkmountpt()?;
		self.init_script()?;
		self.genfstab()?;
		self.instpkgs()?;
		// self.dracut()?;
		self.rootpw()?;
		self.postinst_script()?;

		// self.squashfs()?;
		// self.liveos()?;
		// self.xorriso()?;
		// self.bootloader()?;
		let cfg = self.get_cfg();
		info!("Done: {}.raw", cfg.out);
		Ok(())
	}

	fn bootloader(&self) -> Result<()> {
		match self
			.get_cfg()
			.sys
			.bootloader
			.as_ref()
			.map(|x| x.as_str())
			.unwrap_or(DEFAULT_BOOTLOADER)
		{
			"limine" => self.limine(),
			"grub" => self.grub(),
			x => Err(eyre!("Unknown bootloader: {x}")),
		}
	}
	fn limine(&self) -> Result<()> {
		info!("Installing Limine bootloader");
		let out = &self.get_cfg().out;
		run!("limine", "bios-install", &*format!("{out}.iso"))?;
		Ok(())
	}
	fn grub(&self) -> Result<()> {
		info!("Installing GRUB bootloader");
		// let out = &self.get_cfg().out;
		// self.copy_efi_files(instroot)
		unimplemented!()
	}

	/// Returns volid
	fn liveos(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let distro = &cfg.distro;
		std::fs::create_dir_all(format!("./{distro}/LiveOS"))?;
		std::fs::copy(
			"/usr/share/limine/limine-uefi-cd.bin",
			format!("./{distro}/boot/limine-uefi-cd.bin"),
		)?;
		std::fs::copy(
			"/usr/share/limine/limine-bios-cd.bin",
			format!("./{distro}/boot/limine-bios-cd.bin"),
		)?;
		std::fs::copy(
			"/usr/share/limine/limine-bios.sys",
			format!("./{distro}/boot/limine-bios.sys"),
		)?;
		self.limine_cfg(&*format!("./{distro}/boot/limine.cfg"), distro)?;
		Ok(())
	}

	fn limine_cfg(&self, path: &str, distro: &str) -> Result<()> {
		let cfg = self.get_cfg();
		let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let kver = &Self::get_krnl_ver(root.to_str().unwrap())?;
		let kver = kver.trim_start_matches("kernel-");
		let volid = &cfg.volid;
		let cmdline = cfg.sys.kernel_params.as_ref().map(String::as_str).unwrap_or_default();
		let mut f = std::fs::File::create(path)
			.map_err(|e| eyre!(e).wrap_err("Cannot create limine.cfg"))?;

		f.write_fmt(format_args!("TIMEOUT=5\n\n:{distro}\n\tPROTOCOL=linux\n\t"))?;
		f.write_fmt(format_args!("KERNEL_PATH=boot:///boot/vmlinuz-{kver}\n\t"))?;
		f.write_fmt(format_args!("MODULE_PATH=boot:///boot/initramfs-{kver}.img\n\t"))?;
		f.write_fmt(format_args!(
			"CMDLINE=root=live:LABEL={volid} rd.live.image selinux=0 {cmdline}"
		))?; // maybe enforcing=0
		Ok(())
	}

	fn xorriso(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let distro = &cfg.distro;
		let out = &cfg.out;
		let volid = &cfg.volid;
		info!(out, "Creating ISO");
		run!(~
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
			Path::new(distro).canonicalize()?.to_str().unwrap(),
			"-volid",
			volid,
			"-o",
			&format!("{out}.iso"),
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
		let Some(script) = &cfg.script.postinst else { return Ok(()) };
		let root = &cfg.instroot.canonicalize()?;
		let rootname = root.to_str().unwrap();
		let name = script
			.file_name()
			.ok_or(eyre!("postinst script is not a file"))?
			.to_str()
			.ok_or(eyre!("Cannot get postinst filename in &str"))?;
		let dest = root.join(name);
		debug!(?script, ?dest, "Copying postinst script");
		std::fs::copy(script, &dest)?;
		// debug!("Mounting /dev, /proc, /sys");
		prepare_chroot(rootname)?;
		info!(?script, "Running postinst script");
		// TODO: use unshare
		run!(~"unshare","-R", &rootname, &*format!("/{name}")).map_err(|e| {
			unmount_chroot(rootname).unwrap();
			e.wrap_err("postinst script failed")
		})?;
		debug!(?dest, "Removing postinst script");
		std::fs::remove_file(dest)?;
		// debug!("Unmounting /dev, /proc, /sys");
		unmount_chroot(rootname)?;

		match cfg.format {
			OutputFormat::Disk => {
				// Post-process disk image

				let image = format!("{}.raw", cfg.out);

				// get actual file size by bytes using -du

				let size = cmd_lib::run_fun!(du --block-size=1 $image | cut -f1)?;
				let size = size.parse::<i64>()? + 1;

				// let's truncate the disk image to the actual size
				info!(?size, "Truncating disk image");

				// cmd_lib::run_cmd!(
				// 	truncate -s $size $image;
				// )?;
				cmd_lib::run_cmd!(
					fallocate -d $image;
				)?;
			},
			_ => {},
		}
		Ok(())
	}

	fn rootpw(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let root = &cfg.instroot.canonicalize()?;
		let pw = &*cfg.sys.rootpw;
		info!(pw, "Setting root password");
		let mut fpw = std::fs::File::create(root.join("etc/passwd"))?;
		fpw.write(b"root:x:0:0:root:/root:/bin/sh")?;
		let pw = cmd_lib::run_fun!(mkpasswd $pw)?;
		let mut fsh = std::fs::File::create(root.join("etc/shadow"))?;
		fsh.write_fmt(format_args!("root:{pw}::0:99999:7:::"))?;
		Ok(())
	}

	fn squashfs(&self) -> Result<()> {
		let cfg = self.get_cfg();
		let distro = &cfg.distro;
		let name = format!("./{distro}/LiveOS/squashfs.img");
		let root = &cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
		let root = root.to_str().unwrap();
		let instroot = &cfg.instroot;

		cmd_lib::run_cmd!(
			mkdir -p $distro/boot;
			sh -c "cp $instroot/boot/vmlinuz-* $instroot/boot/initramfs-* $distro/boot/";
		)?;

		info!(name, root, "Squashing fs");

		run!(~"mksquashfs", root, &name, "-comp", "xz", "-noappend", "-Xdict-size", "100%", "-b", "1048576")?;
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

	fn prep_disk(&self) -> Result<()> {
		let cfg = self.get_cfg();

		if let Some(layout) = &cfg.disk {
			// Now let's create a disk file called {out}.raw
			let out_file = format!("{}.raw", cfg.out);

			// Create the disk file

			let disk_size = &layout.disk_size;
			info!(out_file, "Creating disk file");

			cmd_lib::run_cmd!(
				truncate -s $disk_size $out_file;
			)?;

			// Mount disk image to loop device, and return the loop device name

			info!("Mounting disk image to loop device");

			// The reason we run this command instead of just losetup -f is
			// because rustfmt messes up the formatting of the command
			let loop_dev = cmd_lib::run_fun!(bash -c "losetup -f")?;

			debug!("Found loop device: {loop_dev:?}");

			cmd_lib::run_cmd!(
				losetup $loop_dev $out_file --show;
			)?;

			// Partition disk

			info!("Partitioning disk");

			// Create partition table, GPT

			cmd_lib::run_cmd!(
				parted -s $loop_dev mklabel gpt;
			)?;

			// number to track partition number

			let mut part_num = 1;
			let mut efi_num: Option<i32> = None;
			let boot_num: i32;
			let root_num: i32;

			if layout.bootloader {
				// create EFI partition with ESP flag for the first 250MiB
				// label it as EFI

				cmd_lib::run_cmd!(
					parted -s $loop_dev mkpart primary fat32 1MiB 250MiB;
					parted -s $loop_dev set $part_num esp on;
					parted -s $loop_dev name $part_num EFI;
				)?;

				// debug lsblk

				cmd_lib::run_cmd!(
					lsblk;
					partprobe $loop_dev;
					ls -l /dev;
				)?;

				// format EFI partition

				cmd_lib::run_cmd!(
					mkfs.fat -F32 ${loop_dev}p$part_num -n EFI 2>&1;
				)?;
				efi_num = Some(part_num);

				// increment partition number
				part_num += 1;
			}

			// create boot partition for installing kernels with the next 1GiB
			// label as BOOT
			// ext4
			cmd_lib::run_cmd!(
				parted -s $loop_dev mkpart primary ext4 250MiB 1.25GiB;
				parted -s $loop_dev name $part_num BOOT;
			)?;

			cmd_lib::run_cmd!(
				mkfs.ext4 -F ${loop_dev}p$part_num -L BOOT;
			)?;

			boot_num = part_num;

			part_num += 1;

			// Create blank partition with the rest of the free space

			let volid = &cfg.volid;
			cmd_lib::run_cmd!(
				parted -s $loop_dev mkpart primary ext4 1.25GiB 100%;
				parted -s $loop_dev name $part_num $volid;
			)?;

			root_num = part_num;

			// now format the partition

			let root_format = &layout.root_format;

			cmd_lib::run_cmd!(
				mkfs.${root_format} ${loop_dev}p$part_num -L $volid;
			)?;

			// Now, mount them all

			info!("Mounting partitions");

			let instroot = &cfg.instroot.to_str().unwrap_or_default();

			cmd_lib::run_cmd!(
				mkdir -p $instroot;
				mount ${loop_dev}p$root_num $instroot;
				mkdir -p $instroot/boot;
				mount ${loop_dev}p$boot_num $instroot/boot;
			)?;

			if layout.bootloader {
				let efi_num = efi_num.unwrap();
				cmd_lib::run_cmd!(
					mkdir -p $instroot/boot/efi;
					mount ${loop_dev}p$efi_num $instroot/boot/efi;
				)?;
			}

			Ok(())
		} else {
			// error out
			return Err(eyre!("No disk layout specified"));
		}
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
				warn!("{instroot:?} is not empty.");
			}
		} else {
			if instroot.is_file() {
				return Err(eyre!("Cannot make new fs on {instroot:?} because it's a file."));
			}
			std::fs::create_dir(instroot)?;
		}
		match cfg.format {
			OutputFormat::Iso => {
				trace!("Checking for ISO directory");
				std::fs::create_dir_all(format!("./{}/LiveOS", cfg.distro))?;
			},
			OutputFormat::Disk => {
				std::fs::create_dir_all(format!("{}/boot/efi", instroot.display()))?;
				self.prep_disk()?;
			},
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
		// if dnf == "dnf5" {
		// 	pkgs.push("--use-host-config");
		// }

		let mut extra_args = vec![];
		if cfg.arch.is_some() {
			extra_args.push("--forcearch");
			extra_args.push(cfg.arch.as_ref().unwrap());
		}
		prepare_chroot(root).unwrap_or_else(|e| {
			error!(?e, "Failed to prepare chroot");
			std::process::exit(1);
		});
		cmd_lib::run_cmd!(
			$dnf in -y --releasever=$rel $[extra_args] --installroot $root $[pkgs];
			$dnf clean all --installroot $root;
		)
		.unwrap_or_else(|e| {
			error!(?e, "Failed to install packages");
			unmount_chroot(root).unwrap_or_else(|e| {
				error!(?e, "Failed to unmount chroot");
				std::process::exit(1);
			});
			std::process::exit(1);
		});
		unmount_chroot(root)?;
		Ok(())
	}
}

pub struct KatsuCreator {
	cfg: Config,
}
impl From<Config> for KatsuCreator {
	fn from(cfg: Config) -> Self {
		Self { cfg }
	}
}
impl ImageCreator for KatsuCreator {
	// const ARCH: crate::util::Arch = crate::util::Arch::X86;
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

// @madonuko: why? Why did you hardcode everything per architecture? I... My sanity hurts. -@korewaChino
// pub struct LiveImageCreatorX86 {
// 	cfg: Config,
// }

// impl From<Config> for LiveImageCreatorX86 {
// 	fn from(cfg: Config) -> Self {
// 		Self { cfg }
// 	}
// }

// impl ImageCreator for LiveImageCreatorX86 {
// 	// const ARCH: crate::util::Arch = crate::util::Arch::X86;
// 	const EFI_FILES: &'static [(&'static str, &'static str, bool)] = &[
// 		("/boot/efi/EFI/*/shim%arch%.efi", "/EFI/BOOT/BOOT%arch%.EFI", true),
// 		("/boot/efi/EFI/*/gcd%arch%.efi", "/EFI/BOOT/grub%arch%.efi", true),
// 		("/boot/efi/EFI/*/shimia32.efi", "/EFI/BOOT/BOOTIA32.EFI", false),
// 		("/boot/efi/EFI/*/gcdia32.efi", "/EFI/BOOT/grubia32.efi", false),
// 		("/usr/share/grub/unicode.pf2", "/EFI/BOOT/fonts/", true),
// 	];

// 	#[inline]
// 	fn get_cfg(&self) -> &Config {
// 		&self.cfg
// 	}
// }
// pub struct LiveImageCreatorX86_64 {
// 	cfg: Config,
// }

// impl From<Config> for LiveImageCreatorX86_64 {
// 	fn from(cfg: Config) -> Self {
// 		Self { cfg }
// 	}
// }

// impl ImageCreator for LiveImageCreatorX86_64 {
// 	// const ARCH: crate::util::Arch = crate::util::Arch::X86_64;
// 	const EFI_FILES: &'static [(&'static str, &'static str, bool)] = &[
// 		("/boot/efi/EFI/*/shim%arch%.efi", "/EFI/BOOT/BOOT%arch%.EFI", true),
// 		("/boot/efi/EFI/*/gcd%arch%.efi", "/EFI/BOOT/grub%arch%.efi", true),
// 		("/boot/efi/EFI/*/shimia32.efi", "/EFI/BOOT/BOOTIA32.EFI", false),
// 		("/boot/efi/EFI/*/gcdia32.efi", "/EFI/BOOT/grubia32.efi", false),
// 		("/usr/share/grub/unicode.pf2", "/EFI/BOOT/fonts/", true),
// 	];

// 	fn get_cfg(&self) -> &Config {
// 		&self.cfg
// 	}
// }

/// Prepare chroot by mounting /dev, /proc, /sys
fn prepare_chroot(root: &str) -> Result<()> {
	cmd_lib::run_cmd! (
		mkdir -p $root/proc;
		mount -t proc proc $root/proc;
		mkdir -p $root/sys;
		mount -t sysfs sys $root/sys;
		mkdir -p $root/dev;
		mount -o bind /dev $root/dev;
		mkdir -p $root/dev/pts;
		mount -o bind /dev $root/dev/pts;
		sh -c "mv $root/etc/resolv.conf $root/etc/resolv.conf.bak || true";
		cp /etc/resolv.conf $root/etc/resolv.conf;
	)?;
	Ok(())
}

/// Unmount /dev, /proc, /sys
fn unmount_chroot(root: &str) -> Result<()> {
	cmd_lib::run_cmd! (
		umount $root/dev/pts;
		umount $root/dev;
		umount $root/sys;
		umount $root/proc;
		sh -c "mv $root/etc/resolv.conf.bak $root/etc/resolv.conf || true";
	)?;
	Ok(())
}
