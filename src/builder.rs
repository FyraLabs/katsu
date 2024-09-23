use crate::{
	bail_let,
	cli::{OutputFormat, SkipPhases},
	config::{Manifest, Script},
	env_flag,
	util::{just_write, loopdev_with_file},
};
use cmd_lib::{run_cmd, run_fun};
use color_eyre::{eyre::bail, Result};
use indexmap::IndexMap;
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

const WORKDIR: &str = "katsu-work";
crate::prepend_comment!(GRUB_PREPEND_COMMENT: "/boot/grub/grub.cfg", "Grub configurations", katsu::builder::Bootloader::cp_grub);
crate::prepend_comment!(LIMINE_PREPEND_COMMENT: "/boot/limine.cfg", "Limine configurations", katsu::builder::Bootloader::cp_limine);

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub enum Bootloader {
	#[default]
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
			Self::Limine => cmd_lib::run_cmd!(limine bios-install $image 2>&1)?,
			Self::SystemdBoot => cmd_lib::run_cmd!(bootctl --image=$image install 2>&1)?,
		}
		Ok(())
	}
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
		crate::tpl!("limine.cfg.tera" => { LIMINE_PREPEND_COMMENT, distro, vmlinuz, initramfs, cmd, volid } => &limine_cfg);

		let binding = run_fun!(b2sum $limine_cfg)?;
		let liminecfg_b2h = binding.split_whitespace().next().unwrap();

		// enroll limine secure boot
		tracing::info_span!("Enrolling Limine Secure Boot").in_scope(|| -> Result<()> {
			Ok(run_cmd!(
				limine enroll-config $root/boot/limine-uefi-cd.bin $liminecfg_b2h 2>&1;
				limine enroll-config $root/boot/limine-bios.sys $liminecfg_b2h 2>&1;
			)?)
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
		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		cmd_lib::run_cmd!(
			// Format disk with mkfs.fat
			mkfs.msdos $ldp -v -n EFI 2>&1;

			// Mount disk to /tmp/katsu.efiboot
			mkdir -p /tmp/katsu.efiboot;
			mount $ldp /tmp/katsu.efiboot;

			mkdir -p /tmp/katsu.efiboot/EFI/BOOT;
			cp -avr $tree/EFI/BOOT/. /tmp/katsu.efiboot/EFI/BOOT 2>&1;

			umount /tmp/katsu.efiboot;
		)?;

		drop(hdl);
		Ok(())
	}

	fn cp_grub(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		let imgd = chroot.parent().unwrap().join(ISO_TREE);
		let cmd = &manifest.kernel_cmdline.as_ref().map_or("", |s| s);
		let volid = manifest.get_volid();

		let (vmlinuz, initramfs) = self.cp_vmlinuz_initramfs(chroot, &imgd)?;

		let _ = std::fs::remove_dir_all(imgd.join("boot"));
		cmd_lib::run_cmd!(cp -r $chroot/boot $imgd/)?;
		std::fs::rename(imgd.join("boot/grub2"), imgd.join("boot/grub"))?;

		let distro = &manifest.distro.as_ref().map_or("Linux", |s| s);

		crate::tpl!("grub.cfg.tera" => { GRUB_PREPEND_COMMENT, volid, distro, vmlinuz, initramfs, cmd } => imgd.join("boot/grub/grub.cfg"));

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
		cmd_lib::run_cmd!(
			cp -av $imgd/boot/efi/EFI/fedora/. $imgd/EFI/BOOT;
			cp -av $imgd/boot/grub/grub.cfg $imgd/EFI/BOOT/BOOT.conf 2>&1;
			cp -av $imgd/boot/grub/grub.cfg $imgd/EFI/BOOT/grub.cfg 2>&1;
			cp -av $imgd/boot/grub/fonts/unicode.pf2 $imgd/EFI/BOOT/fonts;
			cp -av $imgd/EFI/BOOT/shim${arch_short}.efi $imgd/EFI/BOOT/BOOT${arch_short_upper}.efi;
			cp -av $imgd/EFI/BOOT/shim.efi $imgd/EFI/BOOT/BOOT${arch_32}.efi;
		)?;

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
			"x86_64" => vec!["biosdisk"],
			"aarch64" => vec!["efi_gop"],
			_ => unimplemented!(),
		};

		debug!("Generating Grub images");
		cmd_lib::run_cmd!(
			// todo: uefi support
			grub2-mkimage -O $arch_out -d $chroot/usr/lib/grub/$arch -o $imgd/boot/eltorito.img -p /boot/grub iso9660 $[arch_modules] 2>&1;
			// make it 2.88 MB
			// fallocate -l 1228800 $imgd/boot/eltorito.img;
			// ^ Commented out because it just wiped the entire file - @korewaChino
			// grub2-mkimage -O $arch_64-efi -d $chroot/usr/lib/grub/$arch_64-efi -o $imgd/boot/efiboot.img -p /boot/grub iso9660 efi_gop efi_uga 2>&1;
			grub2-mkrescue -o $imgd/../efiboot.img;
		)?;

		debug!("Copying EFI files from Grub rescue image");
		let (ldp, hdl) = loopdev_with_file(&imgd.join("../efiboot.img"))?;

		cmd_lib::run_cmd!(
			mkdir -p /tmp/katsu-efiboot;
			mount $ldp /tmp/katsu-efiboot;
			cp -r /tmp/katsu-efiboot/boot/grub $imgd/boot/;
			umount /tmp/katsu-efiboot;
		)?;

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

pub trait RootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()>;
}

fn _default_dnf() -> String {
	String::from("dnf")
}

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct DnfRootBuilder {
	#[serde(default = "_default_dnf")]
	pub exec: String,
	#[serde(default)]
	pub packages: Vec<String>,
	#[serde(default)]
	pub options: Vec<String>,
	#[serde(default)]
	pub exclude: Vec<String>,
	#[serde(default)]
	pub releasever: String,
	#[serde(default)]
	pub arch: Option<String>,
	#[serde(default)]
	pub arch_packages: BTreeMap<String, Vec<String>>,
	#[serde(default)]
	pub arch_exclude: BTreeMap<String, Vec<String>>,
	#[serde(default)]
	pub repodir: Option<PathBuf>,
	#[serde(default)]
	pub global_options: Vec<String>,
}

impl RootBuilder for DnfRootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()> {
		info!("Running Pre-install scripts");

		run_all_scripts(&manifest.scripts.pre, chroot, false)?;

		// todo: generate different kind of fstab for iso and other builds
		if let Some(disk) = &manifest.disk {
			// write fstab to chroot
			crate::util::just_write(chroot.join("etc/fstab"), disk.fstab(chroot)?)?;
		}

		let mut packages = self.packages.clone();
		let mut options = self.options.clone();
		let mut exclude = self.exclude.clone();
		let releasever = &self.releasever;

		if let Some(a) = &self.arch {
			debug!(arch = ?a, "Setting arch");
			options.push(format!("--forcearch={a}"));
		}

		if let Some(reposdir) = &self.repodir {
			let reposdir = reposdir.canonicalize()?;
			let reposdir = reposdir.display();
			debug!(?reposdir, "Setting reposdir");
			options.push(format!("--setopt=reposdir={reposdir}"));
		}

		let chroot = chroot.canonicalize()?;

		// Get host architecture using uname
		let host_arch = std::env::consts::ARCH;

		let arch_string = self.arch.as_deref().unwrap_or(host_arch);

		if let Some(pkg) = self.arch_packages.get(arch_string) {
			packages.append(&mut pkg.clone());
		}

		if let Some(pkg) = self.arch_exclude.get(arch_string) {
			exclude.append(&mut pkg.clone());
		}

		let dnf = &self.exec;

		options.append(&mut exclude.iter().map(|p| format!("--exclude={p}")).collect());

		info!("Initializing system with dnf");
		crate::chroot_run_cmd!(&chroot,
			$dnf install -y --releasever=$releasever --installroot=$chroot $[packages] $[options] 2>&1;
			$dnf clean all --installroot=$chroot;
		)?;

		info!("Setting up users");

		if manifest.users.is_empty() {
			warn!("No users specified, no users will be created!");
		} else {
			manifest.users.iter().try_for_each(|user| user.add_to_chroot(&chroot))?;
		}

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		run_all_scripts(&manifest.scripts.post, &chroot, true)
	}
}

#[tracing::instrument(skip(chroot, is_post))]
pub fn run_script(script: Script, chroot: &Path, is_post: bool) -> Result<()> {
	let id = script.id.as_ref().map_or("<NULL>", |s| s);
	bail_let!(Some(mut data) = script.load() => "Cannot load script `{id}`");
	let name = script.name.as_ref().map_or("<Untitled>", |s| s);
	info!(id, name, "Running script");

	let name = format!("script-{}", script.id.as_ref().map_or("untitled", |s| s));
	// check if data has shebang
	if !data.starts_with("#!") {
		warn!("Script does not have shebang, #!/bin/sh will be added. It is recommended to add a shebang to your script.");
		data.insert_str(0, "#!/bin/sh\n");
	}

	if script.chroot.unwrap_or(is_post) {
		just_write(chroot.join("tmp").join(&name), data)?;
		crate::chroot_run_cmd!(chroot,
			chmod +x $chroot/tmp/$name;
			unshare -R $chroot /tmp/$name 2>&1;
			rm -f $chroot/tmp/$name;
		)?;
	} else {
		just_write(PathBuf::from(format!("katsu-work/{name}")), data)?;
		// export envar
		std::env::set_var("CHROOT", chroot);
		cmd_lib::run_cmd!(
			chmod +x katsu-work/$name;
			/usr/bin/env CHROOT=$chroot katsu-work/$name 2>&1;
			rm -f katsu-work/$name;
		)?;
	}

	info!(id, name, "Finished script");
	Ok(())
}

pub fn run_all_scripts(scrs: &[Script], chroot: &Path, is_post: bool) -> Result<()> {
	// name => (Script, is_executed)
	let mut scrs = scrs.to_owned();
	scrs.sort_by_cached_key(|s| s.priority);
	let scrs = scrs.iter().map(|s| (s.id.as_ref().map_or("<?>", |s| s), (s.clone(), false)));
	run_scripts(scrs.collect(), chroot, is_post)
}

#[tracing::instrument]
pub fn run_scripts(
	mut scripts: IndexMap<&str, (Script, bool)>, chroot: &Path, is_post: bool,
) -> Result<()> {
	trace!("Running scripts");
	for idx in scripts.clone().keys() {
		// FIXME: if someone dares to optimize things with unsafe, go for it
		// we can't use get_mut here because we need to do scripts.get_mut() later
		let Some((scr, done)) = scripts.get(idx) else { unreachable!() };
		if *done {
			trace!(idx, "Script is done, skipping");
			continue;
		}

		// Find needs
		let id = scr.id.clone().unwrap_or("<NULL>".into());
		let mut needs = IndexMap::new();
		let scr_needs_vec = &scr.needs.clone();
		for need in scr_needs_vec {
			// when funny rust doesn't know how to convert &String to &str
			bail_let!(Some((s, done)) = scripts.get_mut(need.as_str()) => "Script `{need}` required by `{id}` not found");

			if *done {
				trace!("Script `{need}` (required by `{idx}`) is done, skipping");
				continue;
			}
			needs.insert(need.as_str(), (std::mem::take(s), false));
			*done = true;
		}

		// Run needs
		run_scripts(needs, chroot, is_post)?;

		// Run the actual script
		let Some((scr, done)) = scripts.get_mut(idx) else { unreachable!() };
		run_script(std::mem::take(scr), chroot, is_post)?;
		*done = true;
	}
	Ok(())
}

pub trait ImageBuilder {
	fn build(
		&self, chroot: &Path, image: &Path, manifest: &Manifest, skip_phases: &SkipPhases,
	) -> Result<()>;
}
/// Creates a disk image, then installs to it
#[allow(dead_code)]
pub struct DiskImageBuilder {
	pub image: PathBuf,
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DiskImageBuilder {
	fn build(
		&self, chroot: &Path, image: &Path, manifest: &Manifest, _: &SkipPhases,
	) -> Result<()> {
		// create sparse file on disk
		bail_let!(Some(disk) = &manifest.disk => "Disk layout not specified");
		bail_let!(Some(disk_size) = &disk.size => "Disk size not specified");
		let sparse_path = &image.canonicalize()?.join("katsu.img");
		crate::util::create_sparse(sparse_path, disk_size.as_u64())?;

		// if let Some(disk) = manifest.disk.as_ref() {
		// 	disk.apply(&loopdev.path().unwrap())?;
		// 	disk.mount_to_chroot(&loopdev.path().unwrap(), &chroot)?;
		// 	disk.unmount_from_chroot(&loopdev.path().unwrap(), &chroot)?;
		// }

		let (ldp, hdl) = loopdev_with_file(sparse_path)?;

		// Partition disk
		disk.apply(&ldp, manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH))?;

		// Mount partitions to chroot
		disk.mount_to_chroot(&ldp, chroot)?;

		self.root_builder.build(&chroot.canonicalize()?, manifest)?;

		disk.unmount_from_chroot(chroot)?;

		drop(hdl);
		Ok(())
	}
}

/// Installs directly to a device
#[allow(dead_code)]
pub struct DeviceInstaller {
	pub device: PathBuf,
	pub bootloader: Bootloader,
	// root_builder
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DeviceInstaller {
	fn build(
		&self, _chroot: &Path, _image: &Path, _manifest: &Manifest, _skip_phases: &SkipPhases,
	) -> Result<()> {
		todo!();
		// self.root_builder.build(_chroot, _manifest)?;
		// Ok(())
	}
}

/// Installs as a raw chroot
#[allow(dead_code)]
pub struct FsBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for FsBuilder {
	fn build(
		&self, _chroot: &Path, _image: &Path, manifest: &Manifest, _skip_phases: &SkipPhases,
	) -> Result<()> {
		let out = manifest.out_file.as_ref().map_or("katsu-work/chroot", |s| s);
		let out = Path::new(out);
		// check if image exists, and is a folder
		if out.exists() && !out.is_dir() {
			bail!("Image path is not a directory");
		}

		// if image doesnt exist create it
		if !out.exists() {
			fs::create_dir_all(out)?;
		}

		self.root_builder.build(out, manifest)?;
		Ok(())
	}
}

pub struct IsoBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

const DR_MODS: &str = "livenet dmsquash-live dmsquash-live-ntfs convertfs pollcdrom qemu qemu-net";
const DR_OMIT: &str = "";
const DR_ARGS: &str = "--xz --no-early-microcode";

impl IsoBuilder {
	fn dracut(&self, root: &Path) -> Result<()> {
		info!(?root, "Generating initramfs");
		bail_let!(
			Some(kver) = fs::read_dir(root.join("boot"))?.find_map(|f| {
				// find filename: initramfs-*.img
				trace!(?f, "File in /boot");
				f.ok().and_then(|f| {
					let filename = f.file_name();
					let filename = filename.to_str()?;
					let kver = filename.strip_prefix("initramfs-")?.strip_suffix(".img")?;
					if kver.contains("-rescue-") {
						return None;
					}
					debug!(?kver, "Kernel version");
					Some(kver.to_string())
					// Some(
					// 	f.file_name().to_str()?.rsplit_once('/')?.1.strip_prefix("initramfs-")?.strip_suffix(".img")?.to_string()
					// )
				})
			}) => "Can't find initramfs in /boot."
		);

		// set dracut options
		// this is kind of a hack, but uhh it works maybe
		// todo: make this properly configurable without envvars

		let dr_mods = env_flag!("KATSU_DRACUT_MODS").unwrap_or(DR_MODS.to_string());
		let dr_omit = env_flag!("KATSU_DRACUT_OMIT").unwrap_or(DR_OMIT.to_string());

		let dr_extra_args = env_flag!("KATSU_DRACUT_ARGS").unwrap_or("".to_string());
		let binding = env_flag!("KATSU_DRACUT_ARGS").unwrap_or(DR_ARGS.to_string());
		let dr_basic_args = binding.split(' ').collect::<Vec<_>>();

		// combine them all into one string

		let dr_args2 = vec!["--nomdadmconf", "--nolvmconf", "-fN", "-a", &dr_mods, &dr_extra_args];
		let mut dr_args = vec![];

		dr_args.extend(dr_basic_args);

		dr_args.extend(dr_args2);
		if !dr_omit.is_empty() {
			dr_args.push("--omit");
			dr_args.push(&dr_omit);
		}

		crate::chroot_run_cmd!(root,
			unshare -R $root env - DRACUT_SYSTEMD=0 dracut $[dr_args]
			/boot/initramfs-$kver.img --kver $kver 2>&1;
		)?;
		Ok(())
	}

	pub fn squashfs(&self, chroot: &Path, image: &Path) -> Result<()> {
		// Extra configurable options, for now we use envars
		// todo: document these

		let sqfs_comp = env_flag!("KATSU_SQUASHFS_ARGS").unwrap_or("zstd".to_string());

		info!("Determining squashfs options");

		let sqfs_comp_args = match sqfs_comp.as_str() {
			"gzip" => "-comp gzip -Xcompression-level 9",
			"lzo" => "-comp lzo",
			"lz4" => "-comp lz4 -Xhc",
			"xz" => "-comp xz -Xbcj x86",
			"zstd" => "-comp zstd -Xcompression-level 22",
			"lzma" => "-comp lzma",
			_ => bail!("Unknown squashfs compression: {sqfs_comp}"),
		}
		.split(' ')
		.collect::<Vec<_>>();

		let binding = env_flag!("KATSU_SQUASHFS_ARGS").unwrap_or("".to_string());
		let sqfs_extra_args = binding.split(' ').collect::<Vec<_>>();

		info!("Squashing file system (mksquashfs)");
		cmd_lib::run_cmd!(
			mksquashfs $chroot $image $[sqfs_comp_args] -b 1048576 -noappend
			-e /dev/
			-e /proc/
			-e /sys/
			-p "/dev 755 0 0"
			-p "/proc 755 0 0"
			-p "/sys 755 0 0"
			$[sqfs_extra_args]
		)?;

		Ok(())
	}
	#[allow(dead_code)]
	pub fn erofs(&self, chroot: &Path, image: &Path) -> Result<()> {
		cmd_lib::run_cmd!(mkfs.erofs -d $chroot -o $image)?;
		Ok(())
	}
	// TODO: add mac support
	pub fn xorriso(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		info!("Generating ISO image");
		let volid = manifest.get_volid();
		let (uefi_bin, bios_bin) = self.bootloader.get_bins();
		let tree = chroot.parent().unwrap().join(ISO_TREE);

		// TODO: refactor to new fn in Bootloader
		let grub2_mbr_hybrid = chroot.join("usr/lib/grub/i386-pc/boot_hybrid.img");
		let efiboot = tree.join("boot/efiboot.img");

		match self.bootloader {
			Bootloader::Grub => {
				// cmd_lib::run_cmd!(grub2-mkrescue -o $image $tree -volid $volid 2>&1)?;
				// todo: normal xorriso command does not work for some reason, errors out with some GPT partition shenanigans
				// todo: maybe we need to replicate mkefiboot? (see lorax/efiboot)
				// however, while grub2-mkrescue works, it does not use shim, so we still need to manually call xorriso if we want to use shim
				// - @korewaChino, cc @madomado
				// It works, but we still need to make it use shim somehow
				// ok so, the partition layout should be like this:
				// 1. blank partition with 145,408 bytes
				// 2. EFI partition (fat12)
				// 3. data

				let arch_args = match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH)
				{
					// Hybrid mode is only supported on x86_64
					"x86_64" => vec!["--grub2-mbr", grub2_mbr_hybrid.to_str().unwrap()],
					"aarch64" => vec![],
					_ => unimplemented!(),
				};

				cmd_lib::run_cmd!(xorrisofs -R -V $volid
					$[arch_args]
					-partition_offset 16
					-appended_part_as_gpt
					-append_partition 2 C12A7328-F81F-11D2-BA4B-00A0C93EC93B $efiboot
					-iso_mbr_part_type EBD0A0A2-B9E5-4433-87C0-68B6B72699C7
					-c boot.cat
					--boot-catalog-hide
					-b $bios_bin
					-no-emul-boot
					-boot-load-size 4
					-boot-info-table
					--grub2-boot-info
					-eltorito-alt-boot
					-e --interval:appended_partition_2:all::
					-no-emul-boot
					-vvvvv
					// implant MD5 checksums
					--md5
					// -isohybrid-gpt-basdat
					// -b grub2_mbr=$grub2_mbr_hybrid
					$tree -o $image 2>&1)?;
			},
			_ => {
				debug!("xorriso -as mkisofs --efi-boot {uefi_bin} -b {bios_bin} -no-emul-boot -boot-load-size 4 -boot-info-table --efi-boot {uefi_bin} -efi-boot-part --efi-boot-image --protective-msdos-label {root} -volid KATSU-LIVEOS -o {image}", root = tree.display(), image = image.display());
				cmd_lib::run_cmd!(xorriso -as mkisofs -R --efi-boot $uefi_bin -b $bios_bin -no-emul-boot -boot-load-size 4 -boot-info-table --efi-boot $uefi_bin -efi-boot-part --efi-boot-image --protective-msdos-label $tree -volid $volid -o $image 2>&1)?;
			},
		}

		// implant MD5 checksums
		info!("Implanting MD5 checksums into ISO");
		cmd_lib::run_cmd!(implantisomd5 --force --supported-iso $image)?;
		Ok(())
	}
}

const ISO_TREE: &str = "iso-tree";

impl ImageBuilder for IsoBuilder {
	fn build(
		&self, chroot: &Path, _: &Path, manifest: &Manifest, skip_phases: &SkipPhases,
	) -> Result<()> {
		crate::gen_phase!(skip_phases);
		// You can now skip phases by adding environment variable `KATSU_SKIP_PHASES` with a comma-separated list of phases to skip

		let image = PathBuf::from(manifest.out_file.as_ref().map_or("out.iso", |s| s));
		// Create workspace directory
		let workspace = chroot.parent().unwrap().to_path_buf();
		debug!("Workspace: {workspace:#?}");
		fs::create_dir_all(&workspace)?;

		phase!("root": self.root_builder.build(chroot, manifest));
		// self.root_builder.build(chroot.canonicalize()?.as_path(), manifest)?;

		phase!("dracut": self.dracut(chroot));

		// temporarily store content of iso
		let image_dir = workspace.join(ISO_TREE).join("LiveOS");
		fs::create_dir_all(&image_dir)?;

		phase!("rootimg": self.squashfs(chroot, &image_dir.join("squashfs.img")));

		phase!("copy-live": self.bootloader.copy_liveos(manifest, chroot));

		phase!("iso": self.xorriso(chroot, &image, manifest));

		phase!("bootloader": self.bootloader.install(&image));

		// Reduce storage overhead by removing the original chroot
		// However, we'll keep an env flag to keep the chroot for debugging purposes
		if env_flag!("KATSU_KEEP_CHROOT").is_none() {
			info!("Removing chroot");
			fs::remove_dir_all(chroot)?;
		}

		Ok(())
	}
}

// todo: proper builder struct

pub struct KatsuBuilder {
	pub image_builder: Box<dyn ImageBuilder>,
	pub manifest: Manifest,
	pub skip_phases: SkipPhases,
}

impl KatsuBuilder {
	pub fn new(
		manifest: Manifest, output_format: OutputFormat, skip_phases: SkipPhases,
	) -> Result<Self> {
		let root_builder = match manifest.builder.as_ref().expect("Builder unspecified").as_str() {
			"dnf" => Box::new(manifest.dnf.clone()) as Box<dyn RootBuilder>,
			_ => todo!("builder not implemented"),
		};

		let bootloader = manifest.bootloader.clone();

		let image_builder = match output_format {
			OutputFormat::Iso => {
				Box::new(IsoBuilder { bootloader, root_builder }) as Box<dyn ImageBuilder>
			},
			OutputFormat::DiskImage => Box::new(DiskImageBuilder {
				bootloader,
				root_builder,
				image: PathBuf::from("./katsu-work/image/katsu.img"),
			}) as Box<dyn ImageBuilder>,
			OutputFormat::Folder => {
				Box::new(FsBuilder { bootloader, root_builder }) as Box<dyn ImageBuilder>
			},
			_ => todo!(),
		};

		Ok(Self { image_builder, manifest, skip_phases })
	}

	pub fn build(&self) -> Result<()> {
		let workdir = PathBuf::from(WORKDIR);

		let chroot = workdir.join("chroot");
		fs::create_dir_all(&chroot)?;

		let image = workdir.join("image");
		fs::create_dir_all(&image)?;

		self.image_builder.build(&chroot, &image, &self.manifest, &self.skip_phases)
	}
}
