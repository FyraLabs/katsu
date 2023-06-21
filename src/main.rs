mod cfg;
mod creator;
mod donburi;
mod util;

use cfg::Config;
use color_eyre::Result;
use util::Arch;

use tracing::{debug, trace};

use crate::{
	creator::{LiveImageCreator, LiveImageCreatorX86, LiveImageCreatorX86_64},
	donburi::get_arch,
};

fn main() -> Result<()> {
	color_eyre::install()?;
	tracing_log::LogTracer::init()?;
	let subscriber = tracing_subscriber::FmtSubscriber::builder()
		.with_max_level(get_log_lvl())
		.event_format(tracing_subscriber::fmt::format().pretty())
		.finish();
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
	trace!("カツ丼は最高！");
	for cfg_file in std::env::args().skip(1) {
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
