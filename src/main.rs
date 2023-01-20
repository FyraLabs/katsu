mod cfg;
mod grub;
mod util;

use cfg::Config;
use color_eyre::Result;
use util::Arch;
use std::process::Command;

use tracing::{debug, instrument, trace};

use crate::grub::{LiveImageCreatorX86, LiveImageCreatorX86_64, LiveImageCreator};

fn get_krnl_ver(target: &str) -> Result<String> {
	let out = Command::new("rpm").args(["-q", "kernel", "--root", target]).output()?;
	Ok(String::from_utf8(out.stdout)?.strip_prefix("kernel-").unwrap().to_string())
}

fn get_arch() -> Result<Arch> {
	let out = run!("uname", "-p")?;
	Ok(Arch::from(String::from_utf8(out)?.as_str()))
}

/// ```
/// /usr/bin/dracut --verbose --no-hostonly --no-hostonly-cmdline --install /.profile --add " kiwi-live pollcdrom " --omit " multipath " Ultramarine-Linux.x86_64-0.0.0.initrd 6.0.15-300.fc37.x86_64
/// ```
#[instrument]
fn dracut(cfg: &Config, target: &str) -> Result<()> {
	let raw = &get_krnl_ver(target)?;
	let mut ver = raw.split("-");
	let krnlver = ver.next().unwrap();
	let others = ver.next().unwrap();
	let arch = others.split(".").nth(2).expect("Can't read arch???");
	run!(
		"dracut",
		"--kernel-ver",
		raw,
		"--sysroot",
		target,
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

fn main() -> Result<()> {
	tracing_log::LogTracer::init()?;
	let subscriber = tracing_subscriber::FmtSubscriber::builder()
		.with_max_level(get_log_lvl())
		.event_format(tracing_subscriber::fmt::format().pretty())
		.finish();
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
	trace!("カツ丼は最高！");
	for cfg_file in std::env::args() {
		trace!(cfg_file, "Reading/Parsing config");
		let config: Config = toml::from_str(&std::fs::read_to_string(cfg_file)?)?;
		trace!("Config read done: {config:#?}");
		match get_arch()? {
			Arch::X86 => LiveImageCreatorX86::from(config).exec()?,
			Arch::X86_64 => LiveImageCreatorX86_64::from(config).exec()?,
			Arch::Nyani => panic!("Unknown architecture"),
		}
		
	}
	debug!("Escalate sudo :3");
	sudo::escalate_if_needed().unwrap(); // `Box<dyn Error>` unwrap
	Ok(())
}

fn get_log_lvl() -> tracing_subscriber::filter::LevelFilter {
	use tracing_subscriber::filter::LevelFilter;
	let filter = std::env::var("KATSU_TABEN").unwrap_or("INFO".to_string());
	match filter.as_str() {
		"OFF" => LevelFilter::OFF,
		"ERROR" => LevelFilter::ERROR,
		"WARN" => LevelFilter::WARN,
		"INFO" => LevelFilter::INFO,
		"DEBUG" => LevelFilter::DEBUG,
		"TRACE" => LevelFilter::TRACE,
		_ => LevelFilter::INFO,
	}
}
