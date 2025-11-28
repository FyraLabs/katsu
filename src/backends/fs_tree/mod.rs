pub mod dnf;
pub mod oci;

use crate::config::Manifest;
use std::path::Path;
use color_eyre::Result;

pub trait RootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<()>;
}
