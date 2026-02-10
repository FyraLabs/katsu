pub mod dnf;
pub mod oci;

use crate::config::Manifest;
use color_eyre::Result;
use std::path::{Path, PathBuf};

pub trait RootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<TreeOutput>;
}
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum TreeOutput {
	// tar stream, unused atm
	Tarball(PathBuf),
	Directory(PathBuf),
}
