mod cfg;
mod creator;

mod util;

use cfg::Config;
use color_eyre::Result;
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, Layer};
use util::Arch;

use tracing::trace;

use crate::creator::{LiveImageCreator, LiveImageCreatorX86, LiveImageCreatorX86_64};

fn main() -> Result<()> {
	dotenv::dotenv()?;
	color_eyre::install()?;
	let subscriber =
		tracing_subscriber::Registry::default().with(tracing_error::ErrorLayer::default()).with(
			tracing_subscriber::fmt::layer()
				.pretty()
				.with_filter(tracing_subscriber::EnvFilter::from_env("KATSU_TABEN")),
		);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
	sudo::escalate_if_needed().unwrap();
	trace!("カツ丼は最高！");
	for cfg_file in std::env::args().skip(1) {
		trace!(cfg_file, "Reading/Parsing config");
		let config: Config = toml::from_str(&std::fs::read_to_string(cfg_file)?)?;
		trace!("Config read done: {config:#?}");
		match Arch::get()? {
			Arch::X86 => LiveImageCreatorX86::from(config).exec()?,
			Arch::X86_64 => LiveImageCreatorX86_64::from(config).exec()?,
			Arch::Nyani => panic!("Unknown architecture"),
		}
	}
	Ok(())
}
