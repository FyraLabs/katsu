pub mod dnf;
pub mod oci;

use crate::config::Manifest;
use color_eyre::Result;
use std::path::Path;

pub trait RootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()>;
}
