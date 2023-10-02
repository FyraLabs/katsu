use color_eyre::Result;
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	fs,
	io::{Seek, Write},
	path::PathBuf,
};
use tracing::{debug, info, trace, warn};

use crate::{
	cli::OutputFormat,
	config::{Manifest, Script},
	util,
};
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
	#[serde(default)]
	pub repodir: Option<PathBuf>,
}

impl RootBuilder for DnfRootBuilder {
	fn build(&self, chroot: PathBuf, manifest: &Manifest) -> Result<()> {
		info!("Running Pre-install scripts");

		run_scripts(manifest.scripts.pre.clone(), &chroot, false)?;

		if let Some(disk) = manifest.clone().disk {
			let f = disk.fstab(&chroot)?;
			trace!(fstab = ?f, "fstab");
			// write fstab to chroot
			std::fs::create_dir_all(chroot.join("etc"))?;
			let fstab_path = chroot.join("etc/fstab");
			let mut fstab_file = fs::File::create(fstab_path)?;
			fstab_file.write_all(f.as_bytes())?;
			fstab_file.flush()?;
			drop(fstab_file);
		}

		let mut packages = self.packages.clone();
		let mut options = self.options.clone();
		let exclude = self.exclude.clone();
		let releasever = &self.releasever;

		if let Some(a) = &self.arch {
			debug!(arch = ?a, "Setting arch");
			options.push(format!("--forcearch={a}"));
		}

		if let Some(reposdir) = &self.repodir {
			let reposdir = reposdir.canonicalize()?.display().to_string();
			debug!(reposdir = ?reposdir, "Setting reposdir");
			options.push(format!("--setopt=reposdir={reposdir}"));
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
				dnf install -y --releasever=${releasever} --installroot=${chroot} $[packages] $[options] 2>&1;
				dnf clean all --installroot=${chroot};
			)?;
			Ok(())
		})?;

		info!("Setting up users");

		let mut users = manifest.clone().users;

		if users.is_empty() {
			warn!("No users specified, no users will be created!");
		} else {
			for user in users.iter_mut() {
				user.add_to_chroot(&chroot)?;
			}
		}

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		run_scripts(manifest.scripts.post.clone(), &chroot, true)?;

		Ok(())
	}
}

pub fn run_scripts(scripts: Vec<Script>, chroot: &PathBuf, in_chroot: bool) -> Result<()> {
	for script in scripts {
		if let Some(mut data) = script.load() {
			info!(
				"Running script: {script}",
				script = script.name.as_ref().unwrap_or(&"<Untitled>".to_string())
			);
			info!("Script ID: {id}", id = script.id.as_ref().unwrap_or(&"<NULL>".to_string()));

			let script_name =
				format!("script-{}", script.id.as_ref().unwrap_or(&"untitled".to_string()));
			// check if data has shebang
			if !data.starts_with("#!") {
				// if not, add one
				warn!("Script does not have shebang, #!/bin/sh will be added. It is recommended to add a shebang to your script.");
				data.insert_str(0, "#!/bin/sh\n");
			}
			// write data to chroot
			let fpath = if in_chroot {
				chroot.join("tmp").join(&script_name)
			} else {
				PathBuf::from(format!("katsu-work/{}", &script_name))
			};
			let mut file = fs::File::create(fpath)?;
			file.write_all(data.as_bytes())?;
			file.flush()?;
			drop(file);

			// now add execute bit
			if in_chroot {
				util::run_with_chroot(&chroot, || -> color_eyre::Result<()> {
					cmd_lib::run_cmd!(
						chmod +x ${chroot}/tmp/${script_name};
						unshare -R ${chroot} /tmp/${script_name} 2>&1;
						rm -f ${chroot}/tmp/${script_name};
					)?;
					Ok(())
				})?;
			} else {
				// export envar
				std::env::set_var("CHROOT", chroot);
				cmd_lib::run_cmd!(
					chmod +x katsu-work/${script_name};
					/usr/bin/env CHROOT=${chroot} katsu-work/${script_name} 2>&1;
					rm -f katsu-work/${script_name};
				)?;
			}

			info!(
				"===== Script {script} finished =====",
				script = script.name.as_ref().unwrap_or(&"<Untitled>".to_string())
			);
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

		// Error checking

		if manifest.clone().disk.is_none() {
			// error out
			return Err(color_eyre::eyre::eyre!("Disk layout not specified"));
		} else {
			info!("Disk layout specified");
			if manifest.clone().disk.unwrap().size.is_none() {
				return Err(color_eyre::eyre::eyre!("Disk size not specified"));
			}
		}

		let mut sparse_file = fs::File::create(sparse_path)?;

		let disk_size = manifest.clone().disk.unwrap().size.unwrap();

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

		let disk = manifest.clone().disk.unwrap();

		let ldp = loopdev.path().unwrap();

		// Partition disk

		disk.apply(&ldp)?;

		// Mount partitions to chroot

		disk.mount_to_chroot(&ldp, &chroot)?;

		self.root_builder.build(chroot.clone().canonicalize()?, manifest)?;

		disk.unmount_from_chroot(&ldp, &chroot)?;
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
