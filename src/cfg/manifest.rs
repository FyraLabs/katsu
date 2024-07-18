use std::{fmt::Debug, path::Path};

use bytesize::ByteSize;
use serde::{Deserialize, Serialize};

use super::{partition::PartitionType, script::Script};

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

pub trait BootstrapOption: Debug + dyn_clone::DynClone {
	fn bootstrap_system(&self) -> color_eyre::Result<()>;
}

mod bootstrap_option_serde {
	use super::*;

	pub fn serialize<'se, S>(
		bootstrap_option: &Box<dyn BootstrapOption>, serializer: S,
	) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		todo!()
	}
}

dyn_clone::clone_trait_object!(BootstrapOption);

// todo: rewrite everything
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
	#[must_use]
	pub fn get_volid(&self) -> &str {
		self.iso.as_ref().map_or(DEFAULT_VOLID, |iso| &iso.volume_id)
	}
	/// Load manifest from file
	pub fn load(path: &Path) -> color_eyre::Result<Self> {
		Ok(hcl::de::from_body(ensan::parse(std::fs::read_to_string(path)?)?)?)
	}

	/// Evaluate expressions into a JSON object
	pub fn to_json(&self) -> serde_json::Value {
		serde_json::to_value(self).unwrap()
	}
}

/// Variable types used for validation in `[Var]`
#[derive(Deserialize, Debug, Clone, Serialize)]
pub enum VarType {
	String,
	Int,
	Object,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Var {
	#[serde(rename = "type")]
	pub var_type: VarType,
	pub default: Option<hcl::Value>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub enum BootstrapMethod {
	Oci,
	Tar,
	Dir,
	Squashfs,
	Dnf,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct PartitionLayout {
	pub partition: Vec<Partition>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Partition {
	pub size: ByteSize,
	pub mountpoint: String,
	pub filesystem: String,
	#[serde(rename = "type")]
	pub partition_type: PartitionType,
	pub copy_blocks: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct CopyFiles {
	pub source: String,
	pub destination: String,
}

/// A Katsu output
///
/// Represented by a HCL block `output`
///
/// ```hcl
/// output "type" "id" {}
/// ```
// todo: evaluate dep graph for outputs, so we can build them in the correct order
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
// an output can be a filesystem, a container image, or a disk image.
// It can also rely on other outputs for bootstrapping.
pub struct Output {
	pub id: String,
	/// Method to bootstrap the output filesystem
	pub bootstrap_method: BootstrapMethod,
	/// Copy files from the host to the output tree before packing
	pub copy: Vec<CopyFiles>,
	/// Scripts to run before and after the build
	pub script: Vec<Script>,

	// bootstrapping options
	// todo: make this some kind of enum? or a vec of generic options?
	// Box<dyn BootstrapOption> or something...
	#[serde(with = "bootstrap_option_serde")]
	pub bootstrap: Box<dyn BootstrapOption>,
}
