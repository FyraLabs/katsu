use std::{path::PathBuf, sync::Mutex};

use clap::{Parser, ValueEnum};
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use tracing::trace;

use crate::{builder::KatsuBuilder, config::Manifest};

static CLI_MUTEX: Mutex<Option<KatsuCli>> = Mutex::new(None);

// The structure should be like RPM-OSTree's Compose
// CLI
// so we can do something like
// katsu compose /path/to/manifest.yaml

#[derive(Parser, Debug, Clone)]
#[command(author, version, about)]
pub struct KatsuCli {
	/// Enable verbose output
	#[arg(short, long, default_value = "false")]
	verbose: bool,

	/// Config file location
	config: Option<PathBuf>,

	#[arg(short, long)]
	#[arg(value_enum)]
	/// Format of the artifact Katsu should output
	output: OutputFormat,

	/// Skip individual phases
	///
	/// By default, no phases are skipped for any format
	///
	#[arg(short, long, env = "KATSU_SKIP_PHASES", value_delimiter = ',')]
	pub skip_phases: Vec<String>,

	#[arg(long)]
	/// Override architecture to build for, makes use of DNF's `--arch` option
	/// and chroots using userspace QEMU emulation if necessary
	///
	/// By default, Katsu will build for the host architecture
	arch: Option<String>,

	#[arg(long, short = 'O')]
	/// Override output file location
	output_file: Option<PathBuf>,

	/// Katsu feature flags, comma separated
	#[arg(
		long,
		short = 'X',
		env = "KATSU_FEATURE_FLAGS",
		default_value = "",
		value_delimiter = ','
	)]
	pub feature_flags: Vec<String>,
}

impl KatsuCli {
	// passthrough for clap::Parser::parse
	pub fn p_parse() -> Self {
		CLI_MUTEX.lock().unwrap().get_or_insert_with(Self::parse).clone()
	}
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, ValueEnum)]
pub enum OutputFormat {
	/// Creates a hybrid, bootable ISO-9660 image (with El Torito extensions)
	Iso,
	/// Creates a raw disk image that can either be flashed to a block device,
	/// loopback mounted, or used as a virtual disk image
	DiskImage,
	/// Install to a block device directly
	Device,
	/// Simply copies the root tree to a directory
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

/// Handles the parsed [`Cli`] config.
///
/// # Panics
/// - Cannot escalate sudo
///
/// # Errors
/// - Failed to load manifests (`Manifest::load_all`)
/// - Failed to make new [`KatsuBuilder`]
/// - Failed to build image
pub fn parse(cli: KatsuCli) -> Result<()> {
	// load manifest from config file

	karen::with_env(&["KATSU_LOG"])
		.map_err(|e| color_eyre::eyre::eyre!("Failed to escalate privileges: {e}"))?;

	let config_path = cli.config.as_ref().ok_or_else(|| {
		color_eyre::eyre::eyre!("No config file specified. Please provide a manifest YAML file.")
	})?;

	let mut manifest = Manifest::load_all(config_path, cli.output)?;

	// check for overrides

	if let Some(arch) = cli.arch {
		manifest.dnf.arch = Some(arch);
	}

	if let Some(output_file) = cli.output_file {
		manifest.out_file = Some(output_file.to_string_lossy().to_string());
	}

	trace!(?manifest, "Loaded manifest");

	let builder = KatsuBuilder::new(manifest, cli.output, cli.skip_phases)?;

	tracing::info!("Building image");
	builder.build()?;

	Ok(())
}
