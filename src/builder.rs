use color_eyre::Result;
use serde_derive::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

pub enum Bootloader {
	Limine,
	Grub,
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

		if self.arch.is_some() {
			options.push(format!("--forcearch={}", self.arch.as_ref().unwrap()));
		}

        // Get host architecture using uname
		let host_arch = cmd_lib::run_fun!(uname -m;)?;

		let arch_string = self.arch.as_ref().unwrap_or(&host_arch);

		if self.arch_packages.contains_key(arch_string) {
			packages.append(&mut self.arch_packages.get(arch_string).unwrap().clone());
		}

		for package in exclude {
			options.push(format!("--exclude={}", package));
		}

		cmd_lib::run_cmd!(
			dnf -y --releasever=${releasever} --installroot=${chroot} $[packages] $[options];
			dnf clean all --installroot=${chroot};
		)?;

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
		// Tree file?
		todo!();
		Ok(())
	}
}
