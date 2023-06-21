use crate::cfg::Config;
use crate::util::Arch;
use color_eyre::Result;

use tracing::instrument;

use crate::run;

/// Assume: `target` ends with `/`
pub fn grub_mkconfig(target: &str) -> Result<()> {
	run!("grub2-mkconfig", "-o", &format!("{target}boot/grub2/grub.cfg"))?;
	Ok(())
}
pub fn get_krnl_ver(target: &str) -> Result<String> {
	let out = run!("rpm", "-q", "kernel", "--root", target)?;
	Ok(String::from_utf8(out)?.strip_prefix("kernel-").unwrap().to_string())
}

pub fn get_arch() -> Result<Arch> {
	let out = run!("uname", "-m")?;
	Ok(Arch::from(String::from_utf8(out)?.trim()))
}

/// ```
/// /usr/bin/dracut --verbose --no-hostonly --no-hostonly-cmdline --install /.profile --add " kiwi-live pollcdrom " --omit " multipath " Ultramarine-Linux.x86_64-0.0.0.initrd 6.0.15-300.fc37.x86_64
/// ```
#[instrument]
pub fn dracut(cfg: &Config) -> Result<()> {
	let root = cfg.instroot.canonicalize().expect("Cannot canonicalize instroot.");
	let root = root.to_str().unwrap();
	let raw = &get_krnl_ver(root)?;
	let mut ver = raw.split("-");
	let krnlver = ver.next().unwrap();
	let others = ver.next().unwrap();
	let arch = others.split(".").nth(2).expect("Can't read arch???");
	run!(
		"dracut",
		"--kernel-ver",
		raw,
		"--sysroot",
		&root,
		"--verbose",
		"--no-hostonly",
		"--no-hostonly-cmdline",
		"--install",
		"/.profile",
		"--add",
		" kiwi-live pollcdrom ",
		"--omit",
		" multipath ",
		&format!("{}.{arch}-{krnlver}.initrd", cfg.distro),
		&format!("{krnlver}-{others}"),
	)?;
	Ok(())
}
