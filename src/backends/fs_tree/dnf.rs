//! Use DNF to build an OS root tree from a list of packages.

use crate::{
	backends::{
		bootloader::Bootloader,
		fs_tree::{RootBuilder, TreeOutput},
	},
	builder::run_all_scripts,
	config::Manifest,
};
use color_eyre::{Result, eyre::bail};
use serde::{Deserialize, Serialize};
use std::{
	collections::BTreeMap,
	path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

const DNF_TRANS_COMMENT: &str = "Initial transaction from building with Katsu";

fn _default_dnf() -> String {
	String::from("dnf")
}
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct DnfRootBuilder {
	#[serde(default = "_default_dnf")]
	pub exec: String,
	#[serde(default)]
	pub packages: Vec<String>,
	#[serde(default)]
	pub options: Vec<String>,
	#[serde(default)]
	pub exclude: Vec<String>,
	#[serde(default)]
	pub releasever: String,
	#[serde(default)]
	pub arch: Option<String>,
	#[serde(default)]
	pub arch_packages: BTreeMap<String, Vec<String>>,
	#[serde(default)]
	pub arch_exclude: BTreeMap<String, Vec<String>>,
	#[serde(default)]
	pub repodir: Option<PathBuf>,
	#[serde(default)]
	pub global_options: Vec<String>,
}

// impl DnfRootBuilder {
//     pub fn arch_exclude(&self) -> &BTreeMap<String, Vec<String>> {
//         &self.arch_exclude
//     }
// }

impl RootBuilder for DnfRootBuilder {
	fn build(&self, chroot: &Path, manifest: &Manifest) -> Result<TreeOutput> {
		info!("Running Pre-install scripts");

		run_all_scripts(&manifest.scripts.pre, chroot, false)?;

		// todo: generate different kind of fstab for iso and other builds
		if let Some(disk) = &manifest.disk {
			// write fstab to chroot
			crate::util::just_write(chroot.join("etc/fstab"), disk.fstab(chroot)?)?;
		}

		let mut packages = self.packages.clone();
		let mut options = self.options.clone();
		let mut exclude = self.exclude.clone();
		let releasever = &self.releasever;

		if let Some(a) = &self.arch {
			debug!(arch = ?a, "Setting arch");
			options.push(format!("--forcearch={a}"));
		}

		if let Some(reposdir) = &self.repodir {
			let reposdir = reposdir.canonicalize()?;
			let reposdir = reposdir.display();
			debug!(?reposdir, "Setting reposdir");
			options.push(format!("--setopt=reposdir={reposdir}"));
		}

		let chroot = chroot.canonicalize()?;

		// Get host architecture using uname
		let host_arch = std::env::consts::ARCH;

		let arch_string = self.arch.as_deref().unwrap_or(host_arch);

		if let Some(pkg) = self.arch_packages.get(arch_string) {
			packages.append(&mut pkg.clone());
		}

		if let Some(pkg) = self.arch_exclude.get(arch_string) {
			exclude.append(&mut pkg.clone());
		}

		let dnf = &self.exec;

		options.append(&mut exclude.iter().map(|p| format!("--exclude={p}")).collect());

		info!("Initializing system with dnf");

		// commenting this out for now, chroot mounts shouldn't need to be remade when bootstrapping packages

		// span for DNF stuff
		{
			let chroot = chroot.clone();
			let dnf = dnf.clone();
			let packages = packages.clone();
			let options = options.clone();
			let releasever = releasever.clone();

			let mut cmd = std::process::Command::new(&dnf);
			cmd.arg("do")
				.arg("-y")
				.arg("--action=install")
				.arg(format!("--comment={DNF_TRANS_COMMENT}"))
				.arg("--setopt=tsflags=")
				.arg(format!("--releasever={}", releasever))
				.arg(format!("--installroot={}", chroot.display()))
				.args(&packages)
				.args(&options);
			info!(?cmd, "Running dnf command to install packages");

			let mut clean_cmd = std::process::Command::new(&dnf);
			clean_cmd.arg("clean").arg("all").arg(format!("--installroot={}", chroot.display()));
			crate::util::run_with_chroot(&chroot, || -> Result<()> {
				let status = cmd.status()?;
				if !status.success() {
					bail!("DNF command failed with status: {}", status);
				}
				info!(?clean_cmd, "Running dnf clean command");
				let clean_status = clean_cmd.spawn()?.wait()?;
				if !clean_status.success() {
					warn!("DNF clean command failed with status: {}", clean_status);
				}
				Ok(())
			})?;
		}

		info!("Setting up users");

		if manifest.users.is_empty() {
			warn!("No users specified, no users will be created!");
		} else {
			manifest.users.iter().try_for_each(|user| user.add_to_chroot(&chroot))?;
		}

		if manifest.bootloader == Bootloader::GrubBios || manifest.bootloader == Bootloader::Grub {
			info!("Attempting to run grub2-mkconfig");
			// crate::chroot_run_cmd!(&chroot,
			// 	echo "GRUB_DISABLE_OS_PROBER=true" > /etc/default/grub;
			// )?;

			// While grub2-mkconfig may not return 0 it should still work
			// todo: figure out why it still wouldn't write the file to /boot/grub2/grub.cfg
			//       but works when run inside a post script
			let res = crate::util::enter_chroot_run(&chroot, || {
				std::process::Command::new("grub2-mkconfig")
					.arg("-o")
					.arg("/boot/grub2/grub.cfg")
					.status()?;
				Ok(())
			});

			if let Err(e) = res {
				warn!(?e, "grub2-mkconfig not returning 0, continuing anyway");
			}

			// crate::chroot_run_cmd!(&chroot,
			// 	rm -f /etc/default/grub;
			// )?;
		}

		// now, let's run some funny post-install scripts

		info!("Running post-install scripts");

		let _ = run_all_scripts(&manifest.scripts.post, &chroot, true);

		Ok(TreeOutput::Directory(chroot))
	}
}
