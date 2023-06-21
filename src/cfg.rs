use serde_derive::Deserialize;
use smartstring::alias::String as SStr;

#[derive(Deserialize, Debug)]
pub struct Config {
	pub isodir: String,
	pub distro: String, // "Ultramarine-Linux"
	pub instroot: String,
	pub out: String,
	pub packages: Packages,
	pub sys: System,
	pub fs: FileSystem,
	pub script: std::path::PathBuf,
}

#[derive(Deserialize, Debug)]
pub struct FileSystem {
	pub skip: Option<bool>,
	pub fstype: String,
	pub label: String,
}

#[derive(Deserialize, Debug)]
pub struct Packages {
	pub pkgs: Vec<SStr>,
	pub grps: Vec<SStr>,
}

#[derive(Deserialize, Debug)]
pub struct System {
	pub releasever: u8, // 38
}

// we copy stuff from instroot to isodir
