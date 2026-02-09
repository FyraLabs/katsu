use crate::backends::fs_tree::TreeOutput;
use crate::builder::default_true;
use crate::{backends::fs_tree::RootBuilder, config::Manifest};
use color_eyre::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tracing::info;

/// Metadata for the current image, embedded into derived live images for `bootc install` and debugging
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct BootcImageMetadata {
	/// The original image this was derived from
	pub tag: String,
	/// Image's digest
	pub digest: String,
}

// credits to the Universal Blue people for figuring out how to build a bootc-based image :3
/// A bootc-based image. This is the second implementation of the RootBuilder trait.
/// This takes an OCI image and builds a rootfs out of it, optionally with a containerfile
/// to build a derivation specific to this image.
///
///
/// A derivation is a containerfile with 1 custom argument: `DERIVE_FROM`
///
/// It will be run as `podman build -t <image>:katsu-deriv --build-arg DERIVE_FROM=<image> -f <derivation> <CONTEXT>`
///
/// A containerfile should look like this:
///
/// ```dockerfile
/// ARG DERIVE_FROM
/// FROM $DERIVE_FROM
///
/// RUN echo "Hello from the containerfile!"
/// RUN touch /grass
///
/// # ... Do whatever you want here
/// ```
#[derive(Deserialize, Debug, Clone, Serialize, Default)]
pub struct BootcRootBuilder {
	/// The original image to use as a base
	pub image: String,
	/// Path to a containerfile (Dockerfile) to build a derivation out of
	/// (Optional, if not specified, the image will be used as-is)
	pub derivation: Option<String>,
	pub context: Option<String>,

	#[serde(default = "default_true")]
	pub embed_image: bool,

	// Embed image metadata on derived images
	#[serde(default = "default_true")]
	pub embed_image_metadata: bool,
}

impl RootBuilder for BootcRootBuilder {
	fn build(&self, chroot: &Path, _manifest: &Manifest) -> Result<TreeOutput> {
		let image = &self.image;

		// Pull the image for us
		info!("Loading OCI image");
		cmd_lib::run_cmd!(
			podman pull $image 2>&1;
		)?;
		info!("Current working directory: {}", std::env::current_dir()?.display());

		let context = self.context.as_deref().unwrap_or(".");

		// get pwd
		info!("Building OCI image");
		let d_image = if let Some(derivation) = &self.derivation {
			let og_image = image.split(':').next().unwrap_or(image);
			// get the image, but change the tag to katsu_<variant>
			let deriv = format!("{og_image}:katsu_deriv");

			cmd_lib::run_cmd!(
				podman build -t $deriv --network host --build-arg DERIVE_FROM=$image -f $derivation $context;
			)?;
			deriv
		} else {
			image.to_string()
		};

		info!(?d_image, "Exporting OCI image");
		std::fs::create_dir_all(chroot)?;

		let container = cmd_lib::run_fun!(
			podman create --rm $d_image /bin/bash
		)?;

		cmd_lib::run_cmd!(
			podman export $container | sudo tar -xf - -C $chroot;
		)?;

		// XXX: Wonder if we can use skopeo here instead of podman + tar
		let container_store = chroot.canonicalize()?.join("var/lib/containers/storage");
		let container_store_ovfs = container_store.join("overlay");
		std::fs::create_dir_all(&container_store)?;

		if self.embed_image {
			// redeclare container_store as string, so cmd_lib doesn't complain
			let container_store = container_store.display();
			let container_store_ovfs = container_store_ovfs.display();
			info!(?chroot, ?image, "Copying OCI image to chroot's container store");

			// Push the original image to the chroot's container store, not the derived one
			// If the source reference includes a digest, strip it from the destination reference
			// to avoid digest mismatch errors when writing into containers-storage.
			let dest_image = image.split('@').next().unwrap_or(image);
			cmd_lib::run_cmd!(
				podman push ${image} "containers-storage:[overlay@${container_store}]${dest_image}" --remove-signatures;
			)?;
			// Then we also unmount the thing so it doesn't get in the way
			// but we don't wanna fail entirely if this fails
			cmd_lib::run_cmd!(
				umount -f $container_store_ovfs 2>&1;
			)
			.ok();
		}

		if self.embed_image_metadata {
			let digest = cmd_lib::run_fun!(
				podman inspect --format="{{index .Digest}}" $image
			)?
			.trim()
			.to_string();

			let metadata = BootcImageMetadata { tag: image.to_string(), digest };

			// serialize to yaml
			let serialized = serde_yaml::to_string(&metadata)?;
			tracing::info!(?serialized, "Embedding image metadata into derived image");
			std::fs::write(chroot.join(".bootc_meta.yaml"), serialized)?;
		}

		Ok(TreeOutput::Directory(chroot.to_path_buf()))
	}
}
