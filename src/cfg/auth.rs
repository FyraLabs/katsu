use serde::{Deserialize, Serialize};

const fn _default_true() -> bool {
	true
}

/// Image default users configuration
#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct Auth {
	/// Username for the user
	pub username: String,
	/// Passwords are optional, but heavily recommended
	/// Passwords must be hashed with crypt(3) or mkpasswd(1)
	pub password: Option<String>,
	/// Groups to add the user to
	#[serde(default)]
	pub groups: Vec<String>,
	/// Whether to create a home directory for the user
	/// Defaults to true
	#[serde(default = "_default_true")]
	pub create_home: bool,
	/// Shell for the user
	#[serde(default)]
	pub shell: Option<String>,
	/// UID for the user
	#[serde(default)]
	pub uid: Option<u32>,
	/// GID for the user
	#[serde(default)]
	pub gid: Option<u32>,

	/// SSH keys for the user
	/// This will be written to ~/.`ssh/authorized_keys`
	#[serde(default)]
	pub ssh_keys: Vec<String>,
}

impl Auth {
	/// Converts the Auth struct into a shadowdb entry (/etc/shadow).
	///
	/// May be useful when one wants to write shadowdb manually
	/// instead of imperatively writing commands
	#[must_use]
	pub fn to_shadow(&self) -> String {
		let mut shadow = format!(
			"{}:{}:{}:{}::::::",
			self.username,
			self.password.as_deref().unwrap_or(""),
			self.uid.unwrap_or(0),
			self.gid.unwrap_or(0),
		);
		shadow.truncate(1024);
		shadow
	}

	/// Run command (`useradd`).
	///
	/// # Errors
	/// - happens if the `useradd` command fails.
	pub fn add_user(&self) -> std::io::Result<()> {
		let mut cmd = std::process::Command::new("useradd");
		cmd.arg(&self.username);
		if let Some(shell) = &self.shell {
			cmd.arg("-s").arg(shell);
		}
		if let Some(uid) = &self.uid {
			cmd.arg("-u").arg(uid.to_string());
		}
		if let Some(gid) = &self.gid {
			cmd.arg("-g").arg(gid.to_string());
		}
		if self.create_home {
			cmd.arg("-m");
		}
		cmd.output().map(|_| ())
	}

	#[tracing::instrument]
	pub fn add_to_chroot(&self, chroot: &std::path::Path) -> std::io::Result<()> {
		tracing::debug!("Adding user to chroot");
		tiffin::Container::new(chroot.to_owned()).run(|| self.add_user()).and_then(|r| r)
	}
}
