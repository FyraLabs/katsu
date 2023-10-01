use color_eyre::Result;
use merge_struct::merge;
use serde_derive::{Deserialize, Serialize};
use tracing::debug;
use std::path::PathBuf;

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Manifest {
	pub builder: String,
	#[serde(default)]
	pub import: Vec<PathBuf>,
	/// The distro name for the build result
	// entrypoint must have a distro name
	#[serde(default)]
	pub distro: Option<String>,

	/// Output file name
	// entrypoint must have an output location
	#[serde(default)]
	pub out_file: Option<String>,

	/// DNF configuration
	// todo: dynamically load this?
	pub dnf: crate::builder::DnfRootBuilder,

	#[serde(default)]
	pub scripts: ScriptsManifest,
}

impl Manifest {
	/// Loads a single manifest from a file
	pub fn load(path: PathBuf) -> Result<Self> {
		let mut manifest: Self = serde_yaml::from_str(&std::fs::read_to_string(path.clone())?)?;

		// get dir of path

		let mut path_can = path;
		path_can.pop();

		for import in &mut manifest.import {
			debug!("Import: {import:#?}");
			// swap canonicalized path
			let cn = path_can.join(&import).canonicalize()?;
			debug!("Canonicalized import: {cn:#?}");
			*import = cn;
		}

		// canonicalize all file paths in scripts, then modify their paths put in the manifest

		for script in &mut manifest.scripts.pre {
			if let Some(f) = script.file.as_mut() {
				*f = f.canonicalize()?;
			}
		}

		for script in &mut manifest.scripts.post {
			if let Some(f) = script.file.as_mut() {
				*f = f.canonicalize()?;
			}
		}

		Ok(manifest)
	}

	// pub fn list_all_imports(&self) -> Vec<PathBuf> {
	//     let mut imports = Vec::new();
	//     for import in self.import.clone() {
	//         let mut manifest = Self::load(import.clone()).unwrap();
	//         imports.append(&mut manifest.list_all_imports());
	//         imports.push(import);
	//     }
	//     imports
	// }

	pub fn load_all(path: PathBuf) -> Result<Self> {
		// get all imports, then merge them all
		let mut manifest = Self::load(path.clone())?;

		// get dir of path

		let mut path_can = path.clone();
		path_can.pop();

		for import in manifest.import.clone() {
			let imported_manifest = Self::load_all(import.clone())?;
			manifest = merge(&manifest, &imported_manifest)?;
		}

		Ok(manifest)
	}
}

#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct ScriptsManifest {
	#[serde(default)]
	pub pre: Vec<Script>,
	#[serde(default)]
	pub post: Vec<Script>,
}

#[derive(Deserialize, Debug, Clone, Serialize, PartialEq, Eq)]
// load script from file, or inline if there's one specified
pub struct Script {
	pub id: Option<String>,
	pub name: Option<String>,
	pub file: Option<PathBuf>,
	pub inline: Option<String>,
}

impl Script {
	pub fn load(&self) -> Option<String> {
		if self.inline.is_some() {
			self.inline.clone()
		} else if let Some(f) = &self.file {
			std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok()
		} else {
			self.file
				.as_ref()
				.and_then(|f| std::fs::read_to_string(f.canonicalize().unwrap_or_default()).ok())
		}
	}
}

#[test]
fn test_recurse() {
	// cd tests/ng/recurse

	let manifest = Manifest::load_all(PathBuf::from("tests/ng/recurse/manifest.yaml")).unwrap();

	println!("{manifest:#?}");

	// let ass: Manifest = Manifest { import: vec!["recurse1.yaml", "recurse2.yaml"], distro: Some("RecursiveOS"), out_file: None, dnf: (), scripts: () }
}
