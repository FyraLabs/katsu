mod boot;
mod builder;
mod cli;
mod config;
mod util;

use clap::Parser;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

fn main() -> color_eyre::Result<()> {
	if let Err(e) = dotenvy::dotenv() {
		if !e.not_found() {
			return Err(e.into());
		}
	}

	color_eyre::install()?;
	// default to info level logging, override with KATSU_LOG env var

	let filter = EnvFilter::try_from_env("KATSU_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
	let fmtlyr = fmt::layer().pretty().with_filter(filter);
	let subscriber = Registry::default().with(tracing_error::ErrorLayer::default()).with(fmtlyr);
	tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
	tracing::trace!("カツ丼は最高！");
	let cli = cli::KatsuCli::parse();

	cli::parse(cli)
}
