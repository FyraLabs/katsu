use std::path::PathBuf;

use serde_derive::Deserialize;
use smartstring::alias::String as SStr;

#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
	/// The ISO file will be created.
	/// This is the default.
	#[default]
	Iso,
	/// Generates a disk image
	/// This is not implemented yet.
	Disk,
}

// from string to enum
impl From<&str> for OutputFormat {
	fn from(value: &str) -> Self {
		match value.to_lowercase().as_str() {
			"iso" => Self::Iso,
			"disk" => Self::Disk,
			_ => {
				tracing::warn!("Unknown format: {}, setting ISO mode", value);
				Self::Iso
			}
		}
	}
}

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
	/// The name of the distro / edition.
	/// This is used to name the directory with the ISO content.
	pub distro: String,
	/// The path to the root of the installation.
	/// The directory will be created if it does not exist.
	///
	/// Otherwise, the content inside will remain, which may be
	/// useful for some configuration files that DNF will not overwrite.
	pub instroot: PathBuf,
	/// The path to the ISO file to be created.
	pub out: String,
	/// Packages to be installed onto the new system.
	pub packages: Vec<SStr>,
	/// Configuration for the new system.
	pub sys: System,
	/// Scripts to be run during setup.
	pub script: Script,
	/// The command used for dnf. By default this is "dnf5".
	pub dnf: Option<String>,
	/// The volume id of the ISO file.
	pub volid: String,
	/// The system architecture.
	/// - `x86` (default)
	pub arch: Option<String>,
	/// Output format.
	pub format: OutputFormat,

	/// The disk layout of the new system.
	pub disk: Option<DiskLayout>,
}
#[derive(Deserialize, Debug, Clone)]
pub struct DiskLayout {
	/// Create bootloader partition?
	pub bootloader: bool,
	/// Filesystem of the bootloader partition.
	pub root_format: String,
	/// Total size of the disk image.
	pub disk_size: String,
}


#[derive(Deserialize, Debug, Clone)]
pub struct System {
	/// The release version of the new system.
	pub releasever: u8,
	/// The root password of the new system.
	pub rootpw: SStr,
	/// The bootloader to install.
	/// - `limine` (default)
	/// - `grub`
	pub bootloader: Option<String>,
	/// More kernel parameters.
	/// By default the kernel parameters are:
	/// `root=live:LABEL={volid} rd.live.image selinux=0`
	/// 
	/// If you want to add more parameters after the default ones, use this option.
	pub kernel_params: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Script {
	/// The path to the init script.
	/// The init script is run after the mountpoint is created but before the packages are installed.
	pub init: Option<PathBuf>,
	/// The path to the pre-installation script.
	/// The pre-installation script is run after package installations, `dracut` and root password setup.
	/// `squashfs` will be run after this.
	pub postinst: Option<PathBuf>,
}
