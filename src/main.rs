#![warn(rust_2018_idioms)]

mod builder;
pub mod cfg;
mod util;

use clap::{value_parser, Parser};
use color_eyre::{Report, Result, Section};
use serde_derive::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::trace;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SkipPhases(std::collections::HashSet<String>);

impl SkipPhases {
	#[must_use]
	pub fn contains(&self, phase: &str) -> bool {
		self.0.contains(phase)
	}
}

impl From<&str> for SkipPhases {
	fn from(value: &str) -> Self {
		Self(value.split(',').map(ToOwned::to_owned).collect())
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
			"iso" => Ok(Self::Iso),
			"disk-image" => Ok(Self::DiskImage),
			"device" => Ok(Self::Device),
			"folder" | "fs" => Ok(Self::Folder),
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

/// # Panics
/// - cannot set default subscriber
/// - cannot escalate to sudo
/// - cannot parse `output_file` (not utf-8)
///
/// # Errors
/// - cannot install [`color_eyre`]
/// - cannot read config file
/// - etc.
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

	match cli.config.extension().and_then(|s| s.to_str()) {
		Some("yml" | "yaml") => return Err(
			Report::msg(const_format::formatcp!("Katsu {} does not accept yaml/yml files anymore.", env!("CARGO_PKG_VERSION")))
				.note("Katsu v0.7 supports yaml/yml files. You should downgrade Katsu.")
				.suggestion("You can also port your old Katsu configs into the new HCL format. Please see documentations for more details.")
		),
		Some("hcl") => tracing::info!(cfg=?cli.config, "Loading HCL config file"),
		Some(ext) => tracing::warn!(cfg=?cli.config, ?ext, "Unknown file extension for config file; trying to parse as HCL"),
		None => tracing::warn!(cfg=?cli.config, "Config file does not have any file extensions; trying to parse as HCL"),
	};
	let mut manifest = cfg::manifest::Manifest::load(&cli.config)?;

	// check for overrides

	if let Some(arch) = cli.arch {
		manifest.dnf.arch = Some(arch);
	}

	if let Some(output_file) = cli.output_file {
		manifest.out_file =
			Some(output_file.to_str().expect("Cannot convert output_file to string").to_owned());
	}

	trace!(?manifest, "Loaded manifest");

	let builder = builder::KatsuBuilder::new(manifest, cli.output, cli.skip_phases);

	tracing::info!("Building image");
	builder.build()?;

	Ok(())
}
