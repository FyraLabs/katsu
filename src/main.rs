mod builder;
pub mod cfg;
mod config;
mod util;

use clap::Parser;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct KatsuCli {
	#[arg(short, long, default_value = "false")]
	verbose: bool,

	/// Config file location
	config: Option<PathBuf>,

	#[arg(short, long, value_parser = value_parser!(OutputFormat))]
	output: OutputFormat,
	#[arg(short, long,env = "KATSU_SKIP_PHASES", value_parser = value_parser!(SkipPhases), default_value = "")]
	skip_phases: SkipPhases,

	#[arg(long)]
	/// Override architecture to build for
	arch: Option<String>,

	#[arg(long, short = 'O')]
	/// Override output file location
	output_file: Option<PathBuf>,
}




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
	let cli = KatsuCli::parse();

	sudo::escalate_if_needed().unwrap();

	let mut manifest = Manifest::load_all(&cli.config.unwrap(), cli.output)?;

	// check for overrides

	if let Some(arch) = cli.arch {
		manifest.dnf.arch = Some(arch);
	}

	if let Some(output_file) = cli.output_file {
		manifest.out_file = Some(output_file.into_os_string().into_string().unwrap());
	}

	trace!(?manifest, "Loaded manifest");

	let builder = builder::KatsuBuilder::new(manifest, cli.output, cli.skip_phases)?;

	tracing::info!("Building image");
	builder.build()?;

	Ok(())
}


use std::path::PathBuf;

use clap::{value_parser};
use color_eyre::Result;
use serde_derive::{Deserialize, Serialize};
use tracing::trace;

use crate::{builder::KatsuBuilder, config::Manifest};


#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SkipPhases(Vec<String>);

impl SkipPhases {
	pub fn contains(&self, phase: &str) -> bool {
		self.0.contains(&phase.to_string())
	}
}

impl From<&str> for SkipPhases {
	fn from(value: &str) -> SkipPhases {
		SkipPhases(value.split(',').map(|s| s.to_string()).collect())
	}
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub enum OutputFormat {
	Iso,
	DiskImage,
	Device,
	Folder,
}

impl std::str::FromStr for OutputFormat {
	type Err = String;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"iso" => Ok(OutputFormat::Iso),
			"disk-image" => Ok(OutputFormat::DiskImage),
			"device" => Ok(OutputFormat::Device),
			"folder" => Ok(OutputFormat::Folder),
			"fs" => Ok(OutputFormat::Folder),
			_ => Err(format!("{s} is not a valid output format")),
		}
	}
}


#[tracing::instrument]
pub fn parse(cli: KatsuCli) -> color_eyre::Result<()> {
	sudo::escalate_if_needed().unwrap();

	let mut manifest = Manifest::load_all(&cli.config.unwrap(), cli.output)?;

	// check for overrides

	if let Some(arch) = cli.arch {
		manifest.dnf.arch = Some(arch);
	}

	if let Some(output_file) = cli.output_file {
		manifest.out_file = Some(output_file.into_os_string().into_string().unwrap());
	}

	trace!(?manifest, "Loaded manifest");

	let builder = builder::KatsuBuilder::new(manifest, cli.output, cli.skip_phases)?;

	tracing::info!("Building image");
	builder.build()?;

	Ok(())
}
