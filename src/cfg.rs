use std::path::PathBuf;

use serde_derive::Deserialize;
use smartstring::alias::String as SStr;

#[derive(Deserialize, Debug)]
pub struct Config {
	pub distro: String, // "Ultramarine-Linux"
	pub instroot: PathBuf,
	pub out: String,
	pub packages: Vec<SStr>,
	pub sys: System,
	pub script: Script,
	pub dnf: Option<String>,
	pub volid: String,
}

#[derive(Deserialize, Debug)]
pub struct System {
	pub releasever: u8, // 38
}

#[derive(Deserialize, Debug)]
pub struct Script {
	pub init: Option<PathBuf>,
	pub postinst: Option<PathBuf>,
}

// we copy stuff from instroot to isodir
