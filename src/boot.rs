use std::path::PathBuf;
use color_eyre::Result;

pub trait BootloaderConfig {
    fn config(&self, chroot: PathBuf) -> Result<()>;
}
