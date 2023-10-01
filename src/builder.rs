use color_eyre::Result;
use gpt::{
	disk::{self, DEFAULT_SECTOR_SIZE},
	partition_types::{Type, EFI, LINUX_FS},
};
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	io::{Seek, Write},
	path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

use crate::{chroot_run_cmd, cli::OutputFormat, config::{Manifest, Script}, util};
const WORKDIR: &str = "katsu-work";

pub enum Bootloader {
	Grub,
	Limine,
	SystemdBoot,
}

impl Default for Bootloader {
	fn default() -> Self {
		Self::Grub
	}
}

impl From<&str> for Bootloader {
	fn from(value: &str) -> Self {
		match value.to_lowercase().as_str() {
			"limine" => Self::Limine,
			"grub" => Self::Grub,
			"grub2" => Self::Grub,
			"systemd-boot" => Self::SystemdBoot,
			_ => {
				tracing::warn!("Unknown bootloader: {}, setting GRUB mode", value);
				Self::Grub
			},
		}
	}
}

pub trait RootBuilder {
	fn build(&self, chroot: PathBuf, manifest: &Manifest) -> Result<()>;
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
}

impl RootBuilder for DnfRootBuilder {
	fn build(&self, chroot: PathBuf, manifest: &Manifest) -> Result<()> {

		info!("Running Pre-install scripts");

		run_scripts(manifest.scripts.pre.clone(), &chroot, false)?;

		let mut packages = self.packages.clone();
		let mut options = self.options.clone();
		let exclude = self.exclude.clone();
		let releasever = &self.releasever;

		if let Some(a) = &self.arch {
			options.push(format!("--forcearch={a}"));
		}

		// Get host architecture using uname
		let host_arch = cmd_lib::run_fun!(uname -m;)?;

		let arch_string = self.arch.as_ref().unwrap_or(&host_arch);

		if let Some(pkg) = self.arch_packages.get(arch_string) {
			packages.append(&mut pkg.clone());
		}
		options.append(&mut exclude.iter().map(|p| format!("--exclude={p}")).collect());

		// todo: maybe not unwrap?
		util::run_with_chroot(&chroot, || -> color_eyre::Result<()> {
			cmd_lib::run_cmd!(
				dnf install -y --releasever=${releasever} --installroot=${chroot} $[packages] $[options];
				dnf clean all --installroot=${chroot};
			)?;
			Ok(())
		})?;

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		run_scripts(manifest.scripts.pre.clone(), &chroot, true)?;

		Ok(())
	}
}

pub fn run_scripts(scripts: Vec<Script>, chroot: &PathBuf, in_chroot: bool) -> Result<()> {
	for script in scripts {
		if let Some(mut data) = script.load() {

			info!("Running script: {script}", script = script.name.as_ref().unwrap_or(&"<Untitled>".to_string()));
			info!("Script ID: {id}", id = script.id.as_ref().unwrap_or(&"<NULL>".to_string()));

			// check if data has shebang
			if !data.starts_with("#!") {
				// if not, add one
				warn!("Script does not have shebang, #!/bin/sh will be added. It is recommended to add a shebang to your script.");
				data.insert_str(0, "#!/bin/sh\n");
			}
			// write data to chroot
			let fpath = if in_chroot {
				chroot.join("tmp/script")
			} else {
				PathBuf::from("katsu-work/tmp-script")
			};
			let mut file = fs::File::create(fpath)?;
			file.write_all(data.as_bytes())?;
			file.flush()?;
			drop(file);

			// now add execute bit
			if in_chroot {
				util::run_with_chroot(&chroot, || -> color_eyre::Result<()> {
					cmd_lib::run_cmd!(
						chmod +x ${chroot}/tmp/script;
						chroot ${chroot} /tmp/script;
						rm -f ${chroot}/tmp/script;
					)?;
					Ok(())
				})?;
			} else {
				cmd_lib::run_cmd!(
					chmod +x katsu-work/tmp-script;
					katsu-work/tmp-script;
				)?;
			}

			info!("===== Script {script} finished =====", script = script.name.as_ref().unwrap_or(&"<Untitled>".to_string()));
		}
	}
	Ok(())
}

pub trait ImageBuilder {
	fn build(&self, chroot: PathBuf, image: PathBuf, manifest: &Manifest) -> Result<()>;
}
/// Creates a disk image, then installs to it
pub struct DiskImageBuilder {
	pub image: PathBuf,
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DiskImageBuilder {
	fn build(&self, chroot: PathBuf, image: PathBuf, manifest: &Manifest) -> Result<()> {
		// create sparse file on disk
		let sparse_path = &image.canonicalize()?.join("katsu.img");
		debug!(image = ?sparse_path, "Creating sparse file");
		let mut sparse_file = fs::File::create(sparse_path)?;

		// allocate 8GB (hardcoded for now)
		// todo: unhardcode
		// sparse_file.set_len(8 * 1024 * 1024 * 1024)?;
		sparse_file.seek(std::io::SeekFrom::Start(8 * 1024 * 1024 * 1024))?;
		sparse_file.write_all(&[0])?;

		// sparse_file.flush()?;
		// drop(sparse_file);
		/*
		// use gpt crate to create gpt table
		let mbr = gpt::mbr::ProtectiveMBR::with_lb_size(
			u32::try_from((2 * 1024 * 1024) / 512).unwrap_or(0xFF_FF_FF_FF),
		);
		mbr.overwrite_lba0(&mut sparse_file)?;
		let header =
			gpt::header::read_header_from_arbitrary_device(&mut sparse_file, DEFAULT_SECTOR_SIZE)?;

		let h = header.write_primary(&mut sparse_file, DEFAULT_SECTOR_SIZE)?;

		let mut disk = gpt::GptConfig::new()
			.writable(true)
			.initialized(false)
			.logical_block_size(disk::LogicalBlockSize::Lb512)
			.create_from_device(Box::new(sparse_file), None)?;

		// Create partition table
		// let disk = gpt_cfg.open_from_device(Box::new(sparse_file))?;
		// let mut disk = gpt_cfg.create_from_device(Box::new(sparse_file), None)?;

		debug!(disk = ?disk, "Disk");

		disk.write_inplace()?;

		// create EFI partition (250mb)
		let mut efi_partition = disk.add_partition(
			"EFI",
			250 * 1024 * 1024,
			EFI,
			gpt::partition::PartitionAttributes::all().bits(),
			None,
		)?;

		disk.write_inplace()?;
		// create root partition (rest of disk)

		// get the remaining size of the disk
		let free_sectors = disk.find_free_sectors();
		debug!(free_sectors = ?free_sectors, "Free sectors");

		let mut root_partition = disk.add_partition(
			"ROOT",
			4 * 1024 * 1024 * 1024,
			LINUX_FS,
			gpt::partition::PartitionAttributes::empty().bits(),
			None,
		)?;

		disk.write_inplace()?;

		// disk. */

		// todo: make the above code work
		// for now, we'll just use fdisk and a shell script
		// let sparse_path_str = sparse_path.to_str().unwrap();

		// parted with heredoc

		cmd_lib::run_cmd!(
			parted -s ${sparse_path} mklabel gpt;
			parted -s ${sparse_path} mkpart primary fat32 1MiB 250MiB;
			parted -s ${sparse_path} set 1 esp on;
			parted -s ${sparse_path} name 1 EFI;
			parted -s ${sparse_path} mkpart primary ext4 250MiB 1.25GiB;
			parted -s ${sparse_path} name 2 BOOT;
			parted -s ${sparse_path} mkpart primary ext4 1.25GiB 100%;
			parted -s ${sparse_path} name 3 ROOT;
			parted -s ${sparse_path} print;
		)?;

		// now mount them as loop devices

		let lc = loopdev::LoopControl::open()?;
		let loopdev = lc.next_free()?;

		loopdev.attach_file(&sparse_path)?;

		// scan partitions
		let loopdev_path = loopdev.path().unwrap();
		cmd_lib::run_cmd!(
			partprobe ${loopdev_path};
		)?;

		// Format partitions
		// todo: unhardcode
		cmd_lib::run_cmd!(
			mkfs.vfat -F 32 ${loopdev_path}p1;
			mkfs.ext4 ${loopdev_path}p2;
			mkfs.btrfs ${loopdev_path}p3;
		)?;

		// mount partitions using nix mount

		let efi_loopdev = PathBuf::from(format!("{}p1", loopdev_path.display()));
		let boot_loopdev = PathBuf::from(format!("{}p2", loopdev_path.display()));
		let root_loopdev = PathBuf::from(format!("{}p3", loopdev_path.display()));

		let chroot = chroot.canonicalize()?;

		debug!(
			efi_loopdev = ?efi_loopdev,
			boot_loopdev = ?boot_loopdev,
			root_loopdev = ?root_loopdev,
			chroot = ?chroot,
		);

		let mount_table = [
			(&root_loopdev, &chroot, "btrfs"),
			(&boot_loopdev, &chroot.join("boot"), "ext4"),
			(&efi_loopdev, &chroot.join("boot/efi"), "vfat"),
		];

		// mkdir
		fs::create_dir_all(&chroot)?;

		for (ld, path, fs) in &mount_table {
			debug!(ld = ?ld, path = ?path, fs = ?fs, "Mounting Device");
			fs::create_dir_all(*path)?;
			nix::mount::mount(
				Some(*ld),
				*path,
				Some(*fs),
				nix::mount::MsFlags::empty(),
				None::<&str>,
			)?;
		}

		/* 		nix::mount::mount(
				   Some(&root_loopdev),
				   &chroot,
				   Some("btrfs"),
				   nix::mount::MsFlags::empty(),
				   None::<&str>,
			   )?;

			   fs::create_dir_all(&chroot.join("boot"))?;

			   nix::mount::mount(
				   Some(&boot_loopdev),
				   &chroot.join("boot"),
				   Some("ext4"),
				   nix::mount::MsFlags::empty(),
				   None::<&str>,
			   )?;

			   fs::create_dir_all(&chroot.join("boot/efi"))?;


			   nix::mount::mount(
				   Some(&efi_loopdev),
				   &chroot.join("boot/efi"),
				   Some("vfat"),
				   nix::mount::MsFlags::empty(),
				   None::<&str>,
			   )?;

		*/
		self.root_builder.build(chroot.clone(), manifest)?;


		// Now, after we finally have a rootfs, we can now run some post-install scripts

		

		// reverse mount table, unmount

		for (_, path, _) in mount_table.iter().rev() {
			nix::mount::umount(*path)?;
			// nix::mount::umount(*ld)?;
		}

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
	fn build(&self, chroot: PathBuf, image: PathBuf, manifest: &Manifest) -> Result<()> {
		todo!();
		self.root_builder.build(chroot, manifest)?;
		Ok(())
	}
}

pub struct IsoBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl IsoBuilder {
	pub fn squashfs(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		// todo!();
		cmd_lib::run_cmd!(
			mksquashfs ${chroot} ${image} -comp xz -Xbcj x86 -b 1048576 -noappend;
		)?;
		Ok(())
	}
	pub fn erofs(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		// todo!();
		cmd_lib::run_cmd!(
			mkfs.erofs -d ${chroot} -o ${image};
		)?;
		Ok(())
	}
}

impl ImageBuilder for IsoBuilder {
	fn build(&self, chroot: PathBuf, image: PathBuf, manifest: &Manifest) -> Result<()> {
		// Create workspace directory
		let workspace = chroot.parent().unwrap().to_path_buf();
		debug!("Workspace: {workspace:#?}");
		fs::create_dir_all(workspace.clone())?;
		self.root_builder.build(chroot.clone(), manifest)?;

		// Create image directory
		let image = workspace.join("image");
		fs::create_dir_all(image.clone())?;

		// generate squashfs
		self.squashfs(chroot.clone(), image.clone())?;

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
		let root_builder = match manifest.builder.as_str() {
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
		fs::create_dir_all(workdir.clone())?;

		let chroot = workdir.join("chroot");
		fs::create_dir_all(chroot.clone())?;

		let image = workdir.join("image");
		fs::create_dir_all(image.clone())?;

		self.image_builder.build(chroot, image, &self.manifest)?;

		// chroot_run_cmd!(chroot, unshare -R ${chroot} bash -c "echo woo")?;
		Ok(())
	}
}
