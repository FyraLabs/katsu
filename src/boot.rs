use color_eyre::Result;
use std::path::PathBuf;

pub trait BootloaderConfig {
	fn config(&self, chroot: PathBuf) -> Result<()>;
}
