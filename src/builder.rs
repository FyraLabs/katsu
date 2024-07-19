#![allow(clippy::module_name_repetitions)]
use crate::{
	bail_let,
	cfg::{
		boot::Bootloader,
		manifest::{BuilderType, Manifest},
		script::Script,
	},
	cmd, env_flag,
	util::loopdev_with_file,
	OutputFormat, SkipPhases,
};
use color_eyre::{
	eyre::{bail, eyre},
	Result, Section,
};
use indexmap::IndexMap;
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	path::{Path, PathBuf},
};
use tracing::{debug, info, trace, warn};

const WORKDIR: &str = "katsu-work";

pub trait RootBuilder {
	#[allow(clippy::missing_errors_doc)]
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()>;
}

impl From<BuilderType> for Box<dyn RootBuilder> {
	fn from(value: BuilderType) -> Self {
		match value {
			BuilderType::Dnf => Box::new(DnfRootBuilder::default()),
		}
	}
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
		tiffin::Container::new(chroot.clone()).run(||{
		    let res = cmd!({dnf} "install" "-y" ["--releasever={releasever}"] ["--installroot={chroot:?}"] [[&packages]] [[&options]]).status()?;
			res.success().then(|| cmd!(? {dnf} "clean" "all" ["--installroot={chroot:?}"])).transpose().and_then(|x| x.ok_or_else(|| {
				eyre!("Unknown error while running dracut")
					.wrap_err(res)
					.note(format!("packages: {packages:?}"))
					.note(format!("options: {options:?}"))
			}))
		})??;

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
		let id = scr.id.as_deref().unwrap_or("<NULL>").to_owned();
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
		scr.execute(&mut tiffin::Container::new(chroot.to_path_buf()))?;
		*done = true;
	}
	Ok(())
}

pub trait ImageBuilder {
	#[allow(clippy::missing_errors_doc)]
	fn build(
		&self, chroot: &Path, image: &Path, manifest: &Manifest, skip_phases: &SkipPhases,
	) -> Result<()>;
}
/// Creates a disk image, then installs to it
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

const KATSU_DRACUT_MODS: &str =
	"livenet dmsquash-live dmsquash-live-ntfs convertfs pollcdrom qemu qemu-net";
const KATSU_DRACUT_OMIT: &str = "";
const KATSU_DRACUT_ARGS: &str = "--xz --no-early-microcode";

impl IsoBuilder {
	/// Install dracut.
	///
	/// # Errors
	///
	/// This function will return an error if `dracut` fails.
	fn dracut(root: &Path) -> Result<()> {
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
					Some(kver.to_owned())
				})
			}) => "Can't find initramfs in /boot."
		);

		// set dracut options
		// this is kind of a hack, but uhh it works maybe
		// todo: make this properly configurable without envvars

		let dr_mods = env_flag!(KATSU_DRACUT_MODS);
		let dr_omit = env_flag!(KATSU_DRACUT_OMIT);

		let dr_basic_args = env_flag!(KATSU_DRACUT_ARGS);

		// combine them all into one string

		let dr_args2 = vec!["--nomdadmconf", "--nolvmconf", "-fN", "-a", &dr_mods];
		let mut dr_args = vec![];

		dr_args.extend(dr_basic_args.split(' '));

		dr_args.extend(dr_args2);
		if !dr_omit.is_empty() {
			dr_args.push("--omit");
			dr_args.push(&dr_omit);
		}

		let initramfs = format!("/boot/initramfs-{kver}.img");
		dr_args.extend([&initramfs, "--kver", &kver]);

		let res = tiffin::Container::new(root.to_owned()).run(|| {
			let mut cmd =
				cmd!("unshare" "-R" {{root.display()}} "env" "-" "DRACUT_SYSTEMD=0" "dracut" [[&dr_args]]);
			cmd.status()
		})??;
		res.success().then_some(()).ok_or_else(|| {
			eyre!("Unknown error while running dracut")
				.wrap_err(res)
				.note(format!("Note: dr_args={dr_args:?}"))
		})
	}

	#[tracing::instrument(skip(self))]
	pub fn squashfs(&self, chroot: &Path, image: &Path) -> Result<()> {
		// Extra configurable options, for now we use envars
		// todo: document these

		let sqfs_comp = env_flag!("KATSU_SQUASHFS_ARGS").unwrap_or_else(|| "zstd".to_owned());

		info!("Determining squashfs options");

		let sqfs_comp_args = match sqfs_comp.as_str() {
			"gzip" => "-comp gzip -Xcompression-level 9",
			"lzo" => "-comp lzo",
			"lz4" => "-comp lz4 -Xhc",
			"xz" => "-comp xz -Xbcj x86",
			"zstd" => "-comp zstd -Xcompression-level 19",
			"lzma" => "-comp lzma",
			_ => bail!("Unknown squashfs compression: {sqfs_comp}"),
		}
		.split(' ')
		.collect::<Vec<_>>();

		let sqfs_extra_args = env_flag!("KATSU_SQUASHFS_ARGS").unwrap_or_default();
		let sqfs_extra_args = sqfs_extra_args.split(' ').collect::<Vec<_>>();

		info!("Squashing file system (mksquashfs)");
		let res = cmd!(
			"mksquashfs" {{chroot.display()}} {{image.display()}} [[&sqfs_comp_args]] "-b" "1048576" "-noappend"
			"-e" "/dev/"
			"-e" "/proc/"
			"-e" "/sys/"
			"-p" "/dev 755 0 0"
			"-p" "/proc 755 0 0"
			"-p" "/sys 755 0 0"
			[[&sqfs_extra_args]]
		)
		.status()?;
		res.success().then_some(()).ok_or_else(|| {
			eyre!("Unknown error while running mksquashfs")
				.wrap_err(res)
				.note(format!("sqfs_comp_args: {sqfs_comp_args:?}"))
				.note(format!("sqfs_extra_args: {sqfs_extra_args:?}"))
		})
	}
	/// # Errors
	/// - fail to run `mkfs.erofs`
	#[allow(dead_code)]
	pub fn erofs(&self, chroot: &Path, image: &Path) -> Result<()> {
		cmd!(? "mkfs.erofs" "-d" {{chroot.display()}} "-o" {{image.display()}})
	}
	/// # Errors
	/// - fail to run `xorriso`
	///
	/// # Panics
	/// - fail to parse `chroot` path (not UTF-8)
	/// - `chroot` has no parent dir
	// TODO: add mac support
	#[allow(clippy::unwrap_in_result)]
	pub fn xorriso(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		info!("Generating ISO image");
		let volid = manifest.get_volid();
		let (uefi_bin, bios_bin) = self.bootloader.get_bins();
		let tree = chroot.parent().unwrap().join(ISO_TREE);

		// TODO: refactor to new fn in Bootloader
		let grub2_mbr_hybrid = chroot.join("usr/lib/grub/i386-pc/boot_hybrid.img");
		let efiboot = tree.join("boot/efiboot.img");

		if matches!(self.bootloader, Bootloader::Grub) {
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

			let arch_args = match manifest.dnf.arch.as_deref().unwrap_or(std::env::consts::ARCH) {
				// Hybrid mode is only supported on x86_64
				"x86_64" => vec!["--grub2-mbr", grub2_mbr_hybrid.to_str().unwrap()],
				"aarch64" => vec![],
				_ => unreachable!(),
			};

			// todo: move to partition::Xorriso and Iso9660Table
			let res = cmd!("xorrisofs" "-R" "-V" volid
				[[&arch_args]]
				"-partition_offset" "16"
				"-appended_part_as_gpt"
				"-append_partition" "2" "C12A7328-F81F-11D2-BA4B-00A0C93EC93B" {{efiboot.display()}}
				"-iso_mbr_part_type" "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"
				"-c" "boot.cat"
				"--boot-catalog-hide"
				"-b" bios_bin
				"-no-emul-boot"
				"-boot-load-size" "4"
				"-boot-info-table"
				"--grub2-boot-info"
				"-eltorito-alt-boot"
				"-e" "--interval:appended_partition_2:all::"
				"-no-emul-boot"
				"-vvvvv"
				// implant MD5 checksums
				"--md5"
				// -isohybrid-gpt-basdat
				// -b grub2_mbr=$grub2_mbr_hybrid
				{{tree.display()}} "-o" {{image.display()}}
			)
			.status()?;
			res.success().then_some(()).ok_or_else(|| {
				eyre!("Unknown error while running mksquashfs")
					.wrap_err(res)
					.note(format!("arch_args: {arch_args:?}"))
					.note(format!("efiboot: {efiboot:?}"))
					.note(format!("biosbin: {bios_bin}"))
			})?;
		} else {
			cmd!(?"xorriso" "-as" "mkisofs" "-R" "--efi-boot" uefi_bin "-b" bios_bin "-no-emul-boot" "-boot-load-size" "4" "-boot-info-table" "--efi-boot" uefi_bin "-efi-boot-part" "--efi-boot-image" "--protective-msdos-label" {{tree.display()}} "-volid" volid "-o" {{image.display()}})?;
		}

		// implant MD5 checksums
		info!("Implanting MD5 checksums into ISO");
		cmd!(? "implantisomd5" "--force" "--supported-iso" {{image.display()}})?;
		Ok(())
	}
}

const ISO_TREE: &str = "iso-tree";

impl ImageBuilder for IsoBuilder {
	#![allow(clippy::unwrap_in_result)]
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

		phase!("dracut": Self::dracut(chroot));

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
	pub fn new(manifest: Manifest, output_format: OutputFormat, skip_phases: SkipPhases) -> Self {
		let root_builder = manifest.builder.clone().into();

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

		Self { image_builder, manifest, skip_phases }
	}

	/// # Errors
	/// - IO-errors
	/// - `image_builder` failure
	pub fn build(&self) -> Result<()> {
		let workdir = PathBuf::from(WORKDIR);

		let chroot = workdir.join("chroot");
		fs::create_dir_all(&chroot)?;

		let image = workdir.join("image");
		fs::create_dir_all(&image)?;

		self.image_builder.build(&chroot, &image, &self.manifest, &self.skip_phases)
	}
}
