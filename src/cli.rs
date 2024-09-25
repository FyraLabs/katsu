use std::path::PathBuf;

use clap::{value_parser, Parser, ValueEnum};
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
	#[arg(short, long,env = "KATSU_SKIP_PHASES", value_parser = value_parser!(SkipPhases))]
	#[arg()]
	skip_phases: Option<SkipPhases>,

	#[arg(long)]
	/// Override architecture to build for, makes use of DNF's `--arch` option
	/// and chroots using userspace QEMU emulation if necessary
	/// 
	/// By default, Katsu will build for the host architecture
	arch: Option<String>,

	#[arg(long, short = 'O')]
	/// Override output file location
	output_file: Option<PathBuf>,
}

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
#[tracing::instrument]
pub fn parse(cli: KatsuCli) -> Result<()> {
	// load manifest from config file

	sudo::with_env(&["KATSU_LOG"]).unwrap();

	let mut manifest = Manifest::load_all(&cli.config.unwrap(), cli.output)?;

	// check for overrides

	if let Some(arch) = cli.arch {
		manifest.dnf.arch = Some(arch);
	}

	if let Some(output_file) = cli.output_file {
		manifest.out_file = Some(output_file.into_os_string().into_string().unwrap());
	}

	trace!(?manifest, "Loaded manifest");

	let builder = KatsuBuilder::new(manifest, cli.output, cli.skip_phases.unwrap_or_default())?;

	tracing::info!("Building image");
	builder.build()?;

	Ok(())
}
