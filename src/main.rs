#![warn(clippy::complexity)]
#![warn(clippy::correctness)]
#![warn(clippy::nursery)]
#![warn(clippy::pedantic)]
#![warn(clippy::perf)]
#![warn(clippy::style)]
#![warn(clippy::suspicious)]
// followings are from clippy::restriction
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::missing_safety_doc)]
#![warn(clippy::unwrap_used)]
#![warn(clippy::expect_used)]
#![warn(clippy::format_push_string)]
#![warn(clippy::get_unwrap)]
#![allow(clippy::missing_inline_in_public_items)]
#![allow(clippy::implicit_return)]
#![allow(clippy::blanket_clippy_restriction_lints)]
#![allow(clippy::pattern_type_mismatch)]

mod builder;
pub mod cfg;
mod config;
mod util;

use clap::{value_parser, Parser};
use color_eyre::{Result, Section};
use serde_derive::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::trace;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SkipPhases(std::collections::HashSet<String>);

impl SkipPhases {
	pub fn contains(&self, phase: &str) -> bool {
		self.0.contains(phase)
	}
}

impl From<&str> for SkipPhases {
	fn from(value: &str) -> SkipPhases {
		SkipPhases(value.split(',').map(ToOwned::to_owned).collect())
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

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct KatsuCli {
	/// Config file location
	config: PathBuf,

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

	sudo::escalate_if_needed().expect("Fail to run sudo");

	let cli = KatsuCli::parse();

	let mut manifest = match cli.config.extension().and_then(|s| s.to_str()) {
		Some("yml" | "yaml") => return Err(color_eyre::eyre::eyre!("Katsu {} does not accept yaml/yml files.", env!("CARGO_PKG_VERSION")).note("Katsu v0.7 supports yaml/yml files. You should downgrade Katsu.").suggestion("You can also port your old Katsu configs into the new HCL format. Please see documentations for more details.")),
		Some("hcl") => todo!(),
		Some(ext) => {
			tracing::warn!(cfg=?cli.config, ?ext, "Unknown file extension for config file; trying to parse as HCL");
			cfg::manifest::Manifest::load(&cli.config)?
		},
		None => {
			tracing::warn!(cfg=?cli.config, "Config file does not have any file extensions; trying to parse as HCL");
			cfg::manifest::Manifest::load(&cli.config)?
		},
	};

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
