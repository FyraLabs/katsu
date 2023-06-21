use std::path::PathBuf;

use serde_derive::Deserialize;
use smartstring::alias::String as SStr;

#[derive(Deserialize, Debug)]
pub struct Config {
	pub isodir: String,
	pub distro: String, // "Ultramarine-Linux"
	pub instroot: PathBuf,
	pub out: String,
	pub packages: Vec<SStr>,
	pub sys: System,
	pub fs: FileSystem,
	pub script: Script,
}

#[derive(Deserialize, Debug)]
pub struct FileSystem {
	pub skip: Option<bool>,
	pub fstype: String,
	pub label: String,
}

#[derive(Deserialize, Debug)]
pub struct System {
	pub releasever: u8, // 38
}


#[derive(Deserialize, Debug)]
pub struct Script {
	pub postinit: Option<PathBuf>,
	pub postinst: Option<PathBuf>,
}

// we copy stuff from instroot to isodir
