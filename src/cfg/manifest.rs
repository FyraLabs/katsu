use std::path::Path;

use serde::{Deserialize, Serialize};

const DEFAULT_VOLID: &str = "KATSU-LIVEOS";

fn _default_volid() -> String {
	DEFAULT_VOLID.to_string()
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct IsoConfig {
	/// Volume ID for the ISO image
	#[serde(default = "_default_volid")]
	pub volume_id: String,
}

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuilderType {
	#[default]
	Dnf,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Manifest {
	/// Builder type
	pub builder: BuilderType,
	/// The distro name for the build result
	// entrypoint must have a distro name
	#[serde(default)]
	pub distro: Option<String>,

	/// Output file name
	// entrypoint must have an output location
	#[serde(default)]
	pub out_file: Option<String>,

	#[serde(default)]
	pub disk: Option<super::partition::PartitionLayout>,

	/// DNF configuration
	// todo: dynamically load this?
	#[serde(default)]
	pub dnf: crate::builder::DnfRootBuilder,

	/// Scripts to run before and after the build
	#[serde(default)]
	pub scripts: super::script::ScriptsManifest,

	/// Users to add to the image
	#[serde(default)]
	pub users: Vec<super::auth::Auth>,

	/// Extra parameters to the kernel command line in bootloader configs
	pub kernel_cmdline: Option<String>,

	/// ISO config (optional)
	/// This is only used for ISO images
	#[serde(default)]
	pub iso: Option<IsoConfig>,

	pub bootloader: super::boot::Bootloader,
}

impl Manifest {
	pub fn get_volid(&self) -> &str {
		self.iso.as_ref().map_or(DEFAULT_VOLID, |iso| &iso.volume_id)
	}
	/// Load manifest from file
	pub fn load(path: &Path) -> color_eyre::Result<Self> {
		Ok(hcl::de::from_body(ensan::parse(std::fs::read_to_string(path)?)?)?)
	}
}
