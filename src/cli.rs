use std::path::PathBuf;

use clap::{value_parser, Parser};
use color_eyre::Result;
use serde_derive::{Deserialize, Serialize};
use tracing::trace;

use crate::{builder::KatsuBuilder, config::Manifest};

// The structure should be like RPM-OSTree's Compose
// CLI
// so we can do something like
// katsu compose /path/to/manifest.yaml

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct KatsuCli {
	#[arg(short, long, default_value = "false")]
	verbose: bool,

	/// Config file location
	config: Option<PathBuf>,

	#[arg(short, long, value_parser = value_parser!(OutputFormat))]
	output: OutputFormat,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum OutputFormat {
	Iso,
	DiskImage,
	Device,
}
impl std::str::FromStr for OutputFormat {
	type Err = String;
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"iso" => Ok(OutputFormat::Iso),
			"disk-image" => Ok(OutputFormat::DiskImage),
			"device" => Ok(OutputFormat::Device),
			_ => Err(format!("{s} is not a valid output format")),
		}
	}
}

#[tracing::instrument]
pub fn parse(cli: KatsuCli) -> Result<()> {
	// load manifest from config file

	sudo::escalate_if_needed().unwrap();

	let manifest = Manifest::load_all(&cli.config.unwrap())?;

	trace!(?manifest, "Loaded manifest");

	let builder = KatsuBuilder::new(manifest, cli.output)?;

	tracing::info!("Building image");
	builder.build()?;

	Ok(())
}
