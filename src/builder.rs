use color_eyre::{eyre::eyre, Result};
use serde_derive::{Deserialize, Serialize};
use std::{
	collections::{BTreeMap, HashMap},
	fs,
	io::{Seek, Write},
	path::{PathBuf, Path},
};
use tracing::{debug, info, trace, warn};

use crate::{
	cli::OutputFormat,
	config::{Manifest, Script},
	util,
};
const WORKDIR: &str = "katsu-work";

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

		util::run_with_chroot(chroot, || -> Result<()> {
			cmd_lib::run_cmd!(
				dnf install -y --releasever=$releasever --installroot=$chroot $[packages] $[options];
				dnf clean all --installroot=$chroot;
			)?;
			Ok(())
		})?;

		info!("Setting up users");

		if manifest.users.is_empty() {
			warn!("No users specified, no users will be created!");
		} else {
			manifest.users.iter().try_for_each(|user| user.add_to_chroot(chroot))?;
		}

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		run_all_scripts(&manifest.scripts.post, &chroot, true)?;

		Ok(())
	}
}

pub fn run_script(script: Script, chroot: &Path, in_chroot: bool) -> Result<()> {
	let id = script.id.as_ref().map_or("<NULL>", |s| &**s);
	let Some(mut data) = script.load() else { return Err(eyre!("Cannot load script `{id}`")); };
	let name = script.name.as_ref().map_or("<Untitled>", |s| &**s);
	info!(id, "Running script: {name}");

	let name = format!("script-{}", script.id.as_ref().map_or("untitled", |s| &**s));
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
		util::run_with_chroot(&chroot, || -> Result<()> {
			cmd_lib::run_cmd!(
				chmod +x $chroot/tmp/$name;
				unshare -R $chroot /tmp/$name 2>&1;
				rm -f $chroot/tmp/$name;
			)?;
			Ok(())
		})?;
	} else {
		// export envar
		std::env::set_var("CHROOT", chroot);
		cmd_lib::run_cmd!(
			chmod +x katsu-work/$name;
			/usr/bin/env CHROOT=$chroot katsu-work/$name 2>&1;
			rm -f katsu-work/$name;
		)?;
	}

	info!(
		"===== Script {script} finished =====",
		script = script.name.as_ref().map_or("<Untitled>", |s| &**s)
	);
	Ok(())
}

pub fn run_all_scripts(scripts: &[Script], chroot: &Path, in_chroot: bool) -> Result<()> {
	let mut scrs: HashMap<String, (Script, bool)> = HashMap::new();
	scripts.into_iter().for_each(|s| {
		scrs.insert(s.id.clone().unwrap_or("<?>".into()), (s.clone(), false));
	});
	run_scripts(scrs, chroot, in_chroot)
}

pub fn run_scripts(
	mut scripts: HashMap<String, (Script, bool)>, chroot: &Path, in_chroot: bool,
) -> Result<()> {
	for idx in scripts.clone().keys() {
		// FIXME: if someone dares to optimize things with unsafe, go for it
		// we can't use get_mut here because we need to do scripts.get_mut() later
		let Some((scr, done)) = scripts.get(idx) else { unreachable!() };
		if *done {
			continue;
		}
		let id = scr.id.clone().unwrap_or("<NULL>".into());
		let mut needs = HashMap::new();
		for need in scr.needs.clone() {
			let Some((s, done)) = scripts.get_mut(&need) else {
				return Err(eyre!("Script `{need}` required by `{id}` not found"));
			};
			if *done {
				continue;
			}
			needs.insert(need, (std::mem::take(s), false));
			*done = true;
		}
		run_scripts(needs, chroot, in_chroot)?;
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

		let Some(disk) = &manifest.disk else { 
			return Err(eyre!("Disk layout not specified"));
		};

		let Some(disk_size) = &disk.size else {
			return Err(eyre!("Disk size not specified"));
		};

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
		disk.mount_to_chroot(&ldp, &chroot)?;

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
	fn build(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		todo!();
		self.root_builder.build(&chroot, manifest)?;
		Ok(())
	}
}

pub struct IsoBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl IsoBuilder {
	pub fn squashfs(&self, chroot: &Path, image: &Path) -> Result<()> {
		cmd_lib::run_cmd!(mksquashfs $chroot $image -comp xz -Xbcj x86 -b 1048576 -noappend)?;
		Ok(())
	}
	pub fn erofs(&self, chroot: &Path, image: &Path) -> Result<()> {
		cmd_lib::run_cmd!(mkfs.erofs -d $chroot -o $image)?;
		Ok(())
	}
}

impl ImageBuilder for IsoBuilder {
	fn build(&self, chroot: &Path, image: &Path, manifest: &Manifest) -> Result<()> {
		// Create workspace directory
		let workspace = chroot.parent().unwrap().to_path_buf();
		debug!("Workspace: {workspace:#?}");
		fs::create_dir_all(&workspace)?;
		self.root_builder.build(&chroot, manifest)?;

		// Create image directory
		let image_dir = workspace.join("image");
		fs::create_dir_all(&image_dir)?;

		// generate squashfs
		self.squashfs(&chroot, &image)?;

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
