mod cfg;
mod config;
mod creator;
mod util;
mod builder;
mod boot;

use crate::creator::{ImageCreator, KatsuCreator};
use cfg::Config;
use color_eyre::Result;
use tracing::trace;
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, Layer};

fn main() -> Result<()> {
	dotenvy::dotenv()?;

	color_eyre::install()?;
	let subscriber =
		tracing_subscriber::Registry::default().with(tracing_error::ErrorLayer::default()).with(
			tracing_subscriber::fmt::layer()
				.pretty()
				.with_filter(tracing_subscriber::EnvFilter::from_env("KATSU_LOG")),
		);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
	sudo::escalate_if_needed().unwrap();
	trace!("カツ丼は最高！");
	for cfg_file in std::env::args().skip(1) {
		trace!(cfg_file, "Reading/Parsing config");
		let config: Config = serde_yaml::from_str(&std::fs::read_to_string(cfg_file)?)?;
		trace!("Config read done: {config:#?}");
		// let arch = {
		// 	let cfg_arch = config.clone().arch;

		// 	if cfg_arch.is_none() {
		// 		Arch::get()?
		// 	} else {
		// 		Arch::from(cfg_arch.as_ref().unwrap().as_str())
		// 	}
		// };
		// match arch {
		// 	Arch::X86 => LiveImageCreatorX86::from(config).exec_iso()?,

		// 	Arch::X86_64 => LiveImageCreatorX86_64::from(config).exec_iso()?,

		// 	// todo: please clean this up
		
		// 	Arch::AArch64 => todo!(),

		// 	_ => panic!("Unknown architecture"),
		// }
		KatsuCreator::from(config).exec()?;
	}
	Ok(())
}
