use std::{collections::BTreeMap, fmt::Debug, path::Path};

use bytesize::ByteSize;
use serde::{Deserialize, Serialize};

use super::{
	partition::PartitionType,
	script::{Script, ScriptsManifest},
};

const DEFAULT_VOLID: &str = "KATSU-LIVEOS";

fn _default_volid() -> String {
	DEFAULT_VOLID.to_owned()
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
	use super::BootstrapOption;

	pub fn serialize<'se, S>(
		bootstrap_option: &Box<dyn BootstrapOption>, serializer: S,
	) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		todo!()
	}
}

/// A list of Packages by architecture, used inside `pkg_list`
///
/// A key-value map of `"arch" -> ["package1", "package2"]`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageArchList {
	#[serde(default)]
	#[serde(flatten)]
	pub packages: BTreeMap<String, Vec<String>>,

	#[serde(default)]
	#[serde(rename = "exclude")]
	pub packages_exclude: BTreeMap<String, Vec<String>>,
}

/// A list of packages.
///
/// A key-value list of package specs to pull in.
///
///
/// ```hcl
/// pkg_list "pkgmgr" "listname" {
///    all = ["package1", "package2"]
///   x86_64 = ["package3", "package4"]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageList {
	#[serde(default)]
	#[serde(flatten)]
	// A nested map of package lists by package manager
	// package_manager.listname.arch = ["package1", "package2"]
	pub package_manager: BTreeMap<String, BTreeMap<String, PackageArchList>>,
}

impl PackageList {
	/// Resolve a package list based on the architecture, package manager, and list name.
	///
	/// # Parameters
	/// - `arch`: A string slice that holds the architecture type.
	/// - `pkgmgr`: A string slice that holds the package manager name.
	/// - `list_name`: A string slice that holds the name of the package list.
	///
	/// # Returns
	/// A vector of strings containing the resolved package names. If the package manager or list name
	/// is not found, an empty vector is returned.
	///
	/// # Examples
	/// ```
	/// let package_list = PackageList::new();
	/// let resolved_packages = package_list.resolve("x86_64", "apt", "default");
	/// ```
	#[must_use]
	pub fn resolve(&self, arch: &str, pkgmgr: &str, list_name: &str) -> Vec<String> {
		self.package_manager
			.get(pkgmgr)
			.and_then(|pm| pm.get(list_name))
			.map(|list| {
				let mut pkgs =
					list.packages.get("all").map_or_else(Vec::new, std::clone::Clone::clone);
				if let Some(arch_pkgs) = list.packages.get(arch) {
					pkgs.extend(arch_pkgs.iter().cloned());
				}
				pkgs
			})
			.unwrap_or_default()
	}
}

dyn_clone::clone_trait_object!(BootstrapOption);

// todo: rewrite everything
#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Manifest {
	#[serde(default)]
	#[serde(rename = "pkg_list")]
	pub packages: PackageList,

	// #[serde(default)]
	// #[serde(rename = "var")]
	// pub vars: BTreeMap<String, Var>,
	/// DNF configuration
	// todo: dynamically load this?
	// #[serde(default)]
	// pub dnf: crate::builder::DnfRootBuilder,

	#[serde(default)]
	#[serde(rename = "target")]
	pub targets: BTreeMap<String, Target>,
}

impl Manifest {
	/// Load manifest from file
	///
	/// # Errors
	///
	/// This function will return an error if the file cannot be read or parsed.
	pub fn load(path: &Path) -> color_eyre::Result<Self> {
		Ok(hcl::de::from_body(ensan::parse(std::fs::read_to_string(path)?)?)?)
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

/// A variable
/// ```hcl
/// var "name" {
///    type = "string"
///    default = "value"
/// }
///
/// ```
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub struct PartitionLayout {
	#[serde(default)]
	#[serde(rename = "partition")]
	pub partitions: Vec<Partition>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Partition {

	/// The size of the partition
	/// 
	/// Optional, if not specified, the partition will be the rest of the disk
	#[serde(default)]
	pub size: Option<ByteSize>,

	/// The mountpoint for this partition
	/// 
	/// Optional, if not specified, the partition will not be mounted
	#[serde(default)]
	pub mountpoint: Option<String>,

	/// Filesystem to format the partition with
	/// 
	/// Optional, if not specified, the partition will not be formatted
	#[serde(default)]
	pub filesystem: Option<String>,

	/// GPT/MBR partition type
	#[serde(rename = "type")]
	pub partition_type: Option<PartitionType>,
	/// This partition will be initialized with the block contents of this file
	#[serde(default)]
	pub copy_blocks: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct CopyFiles {
	pub source: String,
	pub destination: String,
}

// a target is a build target, like a make target.
// you need to actually specify how the target is built
// todo: possibly derive a target from another target?
// ``hcl
// target "id" {
//
// }
// ```
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct Target {
	#[serde(rename = "type")]
	pub target_type: String,
	#[serde(default)]
	pub copy_files: Vec<CopyFiles>,
	// insert builder setup manifest here...
	/// Scripts to run before and after the build
	#[serde(default)]
	pub scripts: ScriptsManifest,

	#[serde(default)]
	/// The builder to use for this target
	pub builder: BuilderType,

	#[serde(default)]
	/// References to package lists to merge into the target I guess?
	// todo: implement this
	pub package_lists: Vec<String>,

	#[serde(default)]
	// #[serde(flatten)]
	// partition_layout {
	// 		partition {}
	// }
	pub partition_layout: PartitionLayout,
}
