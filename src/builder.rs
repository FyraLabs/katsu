use color_eyre::Result;
use serde_derive::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};

use crate::{chroot_run_cmd, cli::OutputFormat, config::Manifest, util};
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
	fn build(&self, chroot: PathBuf) -> Result<()>;
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
	fn build(&self, chroot: PathBuf) -> Result<()> {
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

		Ok(())
	}
}

pub trait ImageBuilder {
	fn build(&self, chroot: PathBuf, image: PathBuf) -> Result<()>;
}
/// Creates a disk image, then installs to it
pub struct DiskImageBuilder {
	pub image: PathBuf,
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl ImageBuilder for DiskImageBuilder {
	fn build(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		// do some gpt stuff

		todo!();
		self.root_builder.build(chroot)?;
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
	fn build(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		todo!();
		self.root_builder.build(chroot)?;
		Ok(())
	}
}

pub struct IsoBuilder {
	pub bootloader: Bootloader,
	pub root_builder: Box<dyn RootBuilder>,
}

impl IsoBuilder {
	pub fn squashfs(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		todo!();
		Ok(())
	}
	pub fn erofs(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		todo!();
		Ok(())
	}
}

impl ImageBuilder for IsoBuilder {
	fn build(&self, chroot: PathBuf, image: PathBuf) -> Result<()> {
		// Create workspace directory
		self.root_builder.build(chroot)?;
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

		// self.image_builder.build(chroot.canonicalize()?, image)?;

		chroot_run_cmd!(chroot, unshare -R ${chroot} bash -c "echo woo")?;
		Ok(())
	}
}
