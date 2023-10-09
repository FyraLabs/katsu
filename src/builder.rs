use crate::{
	bail_let,
	cli::OutputFormat,
	config::{Manifest, Script},
};
use color_eyre::{eyre::eyre, Result};
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::{BTreeMap, HashMap},
	fs,
	io::{Seek, Write},
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

const WORKDIR: &str = "katsu-work";
const VOLID: &str = "KATSU-LIVEOS";
crate::prepend_comment!(GRUB_PREPEND_COMMENT: "/etc/default/grub", "Grub default configurations", katsu::builder::Bootloader::cp_grub);
crate::prepend_comment!(LIMINE_PREPEND_COMMENT: "/boot/limine.cfg", "Limine configurations", katsu::builder::Bootloader::cp_limine);

#[derive(Default)]
pub enum Bootloader {
	#[default]
	Grub,
	Limine,
	SystemdBoot,
}

impl From<&str> for Bootloader {
	fn from(value: &str) -> Self {
		match value.to_lowercase().as_str() {
			"limine" => Self::Limine,
			"grub" | "grub2" => Self::Grub,
			"systemd-boot" => Self::SystemdBoot,
			_ => {
				tracing::warn!("Unknown bootloader: {value}, falling back to GRUB");
				Self::Grub
			},
		}
	}
}

impl Bootloader {
	pub fn install(&self, image: &Path) -> Result<()> {
		match *self {
			Self::Grub => cmd_lib::run_cmd!(grub2-install $image 2>&1)?,
			Self::Limine => cmd_lib::run_cmd!(limine bios-install $image 2>&1)?,
			Self::SystemdBoot => cmd_lib::run_cmd!(bootctl --image=$image install 2>&1)?,
		}
		Ok(())
	}
	pub fn get_bins(&self) -> (&'static str, &'static str) {
		match *self {
			Self::Grub => todo!(),
			Self::Limine => ("boot/limine-uefi-cd.bin", "boot/limine-bios-cd.bin"),
			Self::SystemdBoot => todo!(),
		}
	}
	fn cp_vmlinuz_initramfs(&self, chroot: &Path, dest: &Path) -> Result<(String, String)> {
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
			// if let Some((_, s)) = name.rsplit_once('/') {
			// 	if s.starts_with("vmlinuz-") {
			// 		vmlinuz = Some(s.to_string());
			// 	} else if s.starts_with("initramfs-") {
			// 		initramfs = Some(s.to_string());
			// 	}
			// }

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

		std::fs::copy(bootdir.join(&vmlinuz), dest.join("boot").join(&vmlinuz))?;
		std::fs::copy(bootdir.join(&initramfs), dest.join("boot").join(&initramfs))?;

		Ok((vmlinuz, initramfs))
	}

	fn cp_limine(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		// complaint to rust: why can't you coerce automatically with umwrap_or()????
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

		// Generate limine.cfg
		let mut f = std::fs::File::create(root.join("boot/limine.cfg"))
			.map_err(|e| eyre!(e).wrap_err("Cannot create limine.cfg"))?;

		f.write_all(LIMINE_PREPEND_COMMENT.as_bytes())?;
		f.write_fmt(format_args!("TIMEOUT=5\n\n:{distro}\n\tPROTOCOL=linux\n\t"))?;
		f.write_fmt(format_args!("KERNEL_PATH=boot:///boot/{vmlinuz}\n\t"))?;
		f.write_fmt(format_args!("MODULE_PATH=boot:///boot/{initramfs}\n\t"))?;
		f.write_fmt(format_args!(
			"CMDLINE=root=live:LABEL={VOLID} rd.live.image enforcing=0 {cmd}"
		))?;

		Ok(())
	}

	fn cp_grub(&self, manifest: &Manifest, chroot: &Path) -> Result<()> {
		let imgd = chroot.parent().unwrap().join("image/");
		let cmd = &manifest.kernel_cmdline.as_ref().map_or("", |s| s);

		let cfg = std::fs::read_to_string(chroot.join("etc/default/grub"))?;
		let mut f = std::fs::File::create(chroot.join("etc/default/grub"))?;
		f.write_all(GRUB_PREPEND_COMMENT.as_bytes())?;
		for l in cfg.lines() {
			if l.starts_with("GRUB_CMDLINE_LINUX=") {
				f.write_fmt(format_args!(
					"GRUB_CMDLINE_LINUX=\"root=live:LABEL={VOLID} rd.live.image selinux=0 {cmd}\"\n"
				))?;
			} else {
				f.write_all(l.as_bytes())?;
				f.write_all(b"\n")?;
			}
		}
		drop(f); // write and flush changes

		crate::chroot_run_cmd!(chroot, grub2-mkconfig -o /boot/grub2/grub.cfg;)?;
		cmd_lib::run_cmd!(cp -r $chroot/boot $imgd/)?; // too lazy to cp one by one
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

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct DnfRootBuilder {
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
	pub repodir: Option<PathBuf>,
}

impl RootBuilder for DnfRootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()> {
		info!("Running Pre-install scripts");

		run_all_scripts(&manifest.scripts.pre, chroot, false)?;

		// todo: generate different kind of fstab for iso and other builds
		if let Some(disk) = &manifest.disk {
			let f = disk.fstab(chroot)?;
			trace!(fstab = ?f, "fstab");
			// write fstab to chroot
			std::fs::create_dir_all(chroot.join("etc"))?;
			let fstab_path = chroot.join("etc/fstab");
			let mut fstab_file = fs::File::create(fstab_path)?;
			fstab_file.write_all(f.as_bytes())?;
		}

		let mut packages = self.packages.clone();
		let mut options = self.options.clone();
		let exclude = &self.exclude;
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

		// Get host architecture using uname
		let host_arch = cmd_lib::run_fun!(uname -m;)?;

		let arch_string = self.arch.as_ref().unwrap_or(&host_arch);

		if let Some(pkg) = self.arch_packages.get(arch_string) {
			packages.append(&mut pkg.clone());
		}
		options.append(&mut exclude.iter().map(|p| format!("--exclude={p}")).collect());

		crate::chroot_run_cmd!(chroot,
			dnf install -y --releasever=$releasever --installroot=$chroot $[packages] $[options] 2>&1;
			dnf clean all --installroot=$chroot;
		)?;

		info!("Setting up users");

		if manifest.users.is_empty() {
			warn!("No users specified, no users will be created!");
		} else {
			manifest.users.iter().try_for_each(|user| user.add_to_chroot(chroot))?;
		}

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		run_all_scripts(&manifest.scripts.post, chroot, true)?;

		Ok(())
	}
}

#[tracing::instrument(skip(chroot, in_chroot))]
pub fn run_script(script: Script, chroot: &Path, in_chroot: bool) -> Result<()> {
	let id = script.id.as_ref().map_or("<NULL>", |s| &**s);
	bail_let!(Some(mut data) = script.load() => "Cannot load script `{id}`");
	let name = script.name.as_ref().map_or("<Untitled>", |s| s);
	info!(id, name, "Running script");

	let name = format!("script-{}", script.id.as_ref().map_or("untitled", |s| s));
	// check if data has shebang
	if !data.starts_with("#!") {
		warn!("Script does not have shebang, #!/bin/sh will be added. It is recommended to add a shebang to your script.");
		data.insert_str(0, "#!/bin/sh\n");
	}
	// write data to chroot
	let fpath = if in_chroot {
		chroot.join("tmp").join(&name)
	} else {
		PathBuf::from(format!("katsu-work/{name}"))
	};
	fs::File::create(fpath)?.write_all(data.as_bytes())?;

	// now add execute bit
	if in_chroot {
		crate::chroot_run_cmd!(chroot,
			chmod +x $chroot/tmp/$name;
			unshare -R $chroot /tmp/$name 2>&1;
			rm -f $chroot/tmp/$name;
		)?;
	} else {
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

pub fn run_all_scripts(scripts: &[Script], chroot: &Path, in_chroot: bool) -> Result<()> {
	let mut scrs: HashMap<String, (Script, bool)> = HashMap::new();
	scripts.iter().for_each(|s| {
		scrs.insert(s.id.clone().unwrap_or("<?>".into()), (s.clone(), false));
	});
	run_scripts(scrs, chroot, in_chroot)
}

#[tracing::instrument]
pub fn run_scripts(
	mut scripts: HashMap<String, (Script, bool)>, chroot: &Path, in_chroot: bool,
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
		let mut needs = HashMap::new();
		for need in scr.needs.clone() {
			bail_let!(Some((s, done)) = scripts.get_mut(&need) => "Script `{need}` required by `{id}` not found");

			if *done {
				trace!("Script `{need}` (required by `{idx}`) is done, skipping");
				continue;
			}
			needs.insert(need, (std::mem::take(s), false));
			*done = true;
		}

		// Run needs
		run_scripts(needs, chroot, in_chroot)?;

		// Run the actual script
		let Some((scr, done)) = scripts.get_mut(idx) else { unreachable!() };
		run_script(std::mem::take(scr), chroot, in_chroot)?;
		*done = true;
	}
	Ok(())
}

pub trait ImageBuilder {
	fn build(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()>;
}
/// Creates a disk image, then installs to it
pub struct DiskImageBuilder {
	pub image: PathBuf,
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DiskImageBuilder {
	fn build(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		// create sparse file on disk
		let sparse_path = &image.canonicalize()?.join("katsu.img");
		debug!(image = ?sparse_path, "Creating sparse file");

		// Error checking

		let mut sparse_file = fs::File::create(sparse_path)?;

		bail_let!(Some(disk) = &manifest.disk => "Disk layout not specified");
		bail_let!(Some(disk_size) = &disk.size => "Disk size not specified");

		sparse_file.seek(std::io::SeekFrom::Start(disk_size.as_u64()))?;
		sparse_file.write_all(&[0])?;

		// let's mount the disk as a loop device
		let lc = loopdev::LoopControl::open()?;
		let loopdev = lc.next_free()?;

		loopdev.attach_file(sparse_path)?;

		// if let Some(disk) = manifest.disk.as_ref() {
		// 	disk.apply(&loopdev.path().unwrap())?;
		// 	disk.mount_to_chroot(&loopdev.path().unwrap(), &chroot)?;
		// 	disk.unmount_from_chroot(&loopdev.path().unwrap(), &chroot)?;
		// }

		let ldp = loopdev.path().expect("Failed to unwrap loopdev.path() = None");

		// Partition disk
		disk.apply(&ldp)?;

		// Mount partitions to chroot
		disk.mount_to_chroot(&ldp, chroot)?;

		self.root_builder.build(&chroot.canonicalize()?, manifest)?;

		disk.unmount_from_chroot(&ldp, chroot)?;
		loopdev.detach()?;

		Ok(())
	}
}

/// Installs directly to a device
pub struct DeviceInstaller {
	pub device: PathBuf,
	pub bootloader: Bootloader,
	// root_builder
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DeviceInstaller {
	fn build(&self, _chroot: &Path, _image: &Path, _manifest: &Manifest) -> Result<()> {
		todo!();
		self.root_builder.build(_chroot, _manifest)?;
		Ok(())
	}
}

pub struct IsoBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

const DR_MODS: &str = "livenet dmsquash-live dmsquash-live-ntfs convertfs pollcdrom qemu qemu-net";
const DR_OMIT: &str = "plymouth multipath";

impl IsoBuilder {
	fn dracut(&self, root: &Path) -> Result<()> {
		info!(?root, "Generating initramfs");
		let dir = fs::read_dir(root.join("boot"))?;
		// collect into a vector
		let dir: Vec<_> = dir.collect::<Result<_, _>>()?;
		debug!(?dir, "Files in /boot");
		bail_let!(
			Some(kver) = fs::read_dir(root.join("boot"))?.find_map(|f|
				// find filename: initramfs-*.img
				{
					debug!(?f, "File in /boot");
					f.ok().and_then(|f|{
						let filename = f.file_name();
						let filename = filename.to_str()?;
						let initramfs = filename.strip_prefix("initramfs-")?.strip_suffix(".img")?.to_string();
						// remove the last suffix with the arch
						let arch = initramfs.rsplit_once('.')?.1;
						debug!(?arch, "Arch");
						// if arch != "img" {
						// 	return None;
						// }
						// let kver = initramfs.rsplit_once('.')?.0;
						let kver = initramfs;
						debug!(?kver, "Kernel version");
						Some(kver.to_string())
						// Some(
						// 	f.file_name().to_str()?.rsplit_once('/')?.1.strip_prefix("initramfs-")?.strip_suffix(".img")?.to_string()
						// )
					} )
				}
			) => "Can't find initramfs in /boot."
		);

		crate::chroot_run_cmd!(
			root,
			unshare -R $root dracut --xz -vfNa $DR_MODS -o $DR_OMIT --no-early-microcode /boot/initramfs-$kver.img $kver 2>&1;
		)?;
		Ok(())
	}

	pub fn squashfs(&self, chroot: &Path, image: &Path) -> Result<()> {
		cmd_lib::run_cmd!(mksquashfs $chroot $image -comp xz -Xbcj x86 -b 1048576 -noappend)?;
		Ok(())
	}
	pub fn erofs(&self, chroot: &Path, image: &Path) -> Result<()> {
		cmd_lib::run_cmd!(mkfs.erofs -d $chroot -o $image)?;
		Ok(())
	}
	pub fn xorriso(&self, chroot: &Path, image: &Path) -> Result<()> {
		let (uefi_bin, bios_bin) = self.bootloader.get_bins();
		let root = chroot.parent().unwrap().join(ISO_TREE);
		debug!("xorriso -as mkisofs -b {bios_bin} -no-emul-boot -boot-load-size 4 -boot-info-table --efi-boot {uefi_bin} -efi-boot-part --efi-boot-image --protective-msdos-label {root} -volid KATSU-LIVEOS -o {image}", bios_bin = bios_bin, uefi_bin = uefi_bin, root = root.display(), image = image.display());
		cmd_lib::run_cmd!(xorriso -as mkisofs -b $bios_bin -no-emul-boot -boot-load-size 4 -boot-info-table --efi-boot $uefi_bin -efi-boot-part --efi-boot-image --protective-msdos-label $root -volid $VOLID -o $image 2>&1)?;
		Ok(())
	}
}

const ISO_TREE: &str = "iso-tree";

impl ImageBuilder for IsoBuilder {
	fn build(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		// Create workspace directory
		let workspace = chroot.parent().unwrap().to_path_buf();
		debug!("Workspace: {workspace:#?}");
		fs::create_dir_all(&workspace)?;
		self.root_builder.build(chroot.canonicalize()?.as_path(), manifest)?;

		self.dracut(chroot)?;

		// temporarily store content of iso
		let image_dir = workspace.join(ISO_TREE).join("LiveOS");
		fs::create_dir_all(&image_dir)?;

		// todo: fix the paths
		// the ISO working tree should be in katsu-work/iso-tree
		// the image output would be in katsu-work/image

		// generate squashfs
		self.squashfs(chroot, &image_dir.join("squashfs.img"))?;

		self.bootloader.copy_liveos(manifest, chroot)?;
		let image = format!("{}/katsu.iso", image.display());
		let path = PathBuf::from(image);

		self.xorriso(chroot, path.as_path())?;
		self.bootloader.install(path.as_path())?;

		Ok(())
	}
}

// todo: proper builder struct

pub struct KatsuBuilder {
	pub image_builder: Box<dyn ImageBuilder>,
	pub manifest: Manifest,
}

impl KatsuBuilder {
	pub fn new(manifest: Manifest, output_format: OutputFormat) -> Result<Self> {
		let root_builder = match manifest.builder.clone().expect("A valid builder value").as_str() {
			"dnf" => Box::new(manifest.dnf.clone()) as Box<dyn RootBuilder>,
			_ => todo!("builder not implemented"),
		};

		let image_builder = match output_format {
			OutputFormat::Iso => {
				Box::new(IsoBuilder { bootloader: Bootloader::Limine, root_builder })
					as Box<dyn ImageBuilder>
			},
			OutputFormat::DiskImage => Box::new(DiskImageBuilder {
				bootloader: Bootloader::Limine,
				root_builder,
				image: PathBuf::from("./katsu-work/image/katsu.img"),
			}) as Box<dyn ImageBuilder>,
			_ => todo!(),
		};

		Ok(Self { image_builder, manifest })
	}

	pub fn build(&self) -> Result<()> {
		let workdir = PathBuf::from(WORKDIR);
		fs::create_dir_all(&workdir)?;

		let chroot = workdir.join("chroot");
		fs::create_dir_all(&chroot)?;

		let image = workdir.join("image");
		fs::create_dir_all(&image)?;

		self.image_builder.build(&chroot, &image, &self.manifest)?;

		// chroot_run_cmd!(chroot, unshare -R ${chroot} bash -c "echo woo")?;
		Ok(())
	}
}
