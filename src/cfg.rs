use std::path::PathBuf;

use serde_derive::Deserialize;
use smartstring::alias::String as SStr;

#[derive(Deserialize, Debug)]
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
}

#[derive(Deserialize, Debug)]
pub struct System {
	/// The release version of the new system.
	pub releasever: u8,
	/// The root password of the new system.
	pub rootpw: SStr,
}

#[derive(Deserialize, Debug)]
pub struct Script {
	/// The path to the init script.
	/// The init script is run after the mountpoint is created but before the packages are installed.
	pub init: Option<PathBuf>,
	/// The path to the pre-installation script.
	/// The pre-installation script is run after package installations, `dracut` and root password setup.
	/// `squashfs` will be run after this.
	pub postinst: Option<PathBuf>,
}
