use serde_derive::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Config {
	pub isodir: String,
	pub distro: String, // "Ultramarine-Linux"
	pub instroot: String,
}
