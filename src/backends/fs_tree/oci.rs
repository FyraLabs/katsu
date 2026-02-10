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

	#[serde(default)]
	pub embed_extra_images: Vec<String>,
}

impl BootcRootBuilder {
	/// Embeds an OCI image into the container store
	fn embed_image_to_store(
		image: &str, container_store: &Path, container: &str,
	) -> Result<std::path::PathBuf> {
		let container_store_display = container_store.display();
		info!(?image, "Copying OCI image to chroot's container store");

		// Create a temporary storage.conf in /run that uses fuse-overlayfs for nested overlay support
		let storage_conf_path = Path::new("/run").join("katsu-storage.conf");
		let storage_conf = r#"[storage]
driver = "overlay"

[storage.options]
mount_program = "/usr/bin/fuse-overlayfs"

[storage.options.overlay]
mount_program = "/usr/bin/fuse-overlayfs"
"#
		.to_string();
		std::fs::write(&storage_conf_path, storage_conf)?;

		// Use skopeo to copy the image from containers-storage to the destination
		// skopeo handles copying layers properly between different storage configurations
		let dest_image = image.split('@').next().unwrap_or(image);
		let storage_conf_env = storage_conf_path.display();
		cmd_lib::run_cmd!(
			CONTAINERS_STORAGE_CONF=${storage_conf_env} skopeo copy --dest-compress --remove-signatures "containers-storage:${image}" "containers-storage:[${container_store_display}]${dest_image}";
		)?;

		// quirk: After we push the image, podman will unmount the entire container store, so we have to remount it
		let new_mountpoint = cmd_lib::run_fun!(
			podman mount $container
		)?;
		Ok(Path::new(new_mountpoint.trim()).to_path_buf())
	}
}

impl RootBuilder for BootcRootBuilder {
	fn build(&self, chroot: &Path, _manifest: &Manifest) -> Result<TreeOutput> {
		let image = &self.image;

		// Pull the image for us
		info!("Loading OCI images");
		cmd_lib::run_cmd!(
			podman pull $image 2>&1;
		)?;
		for extra_image in &self.embed_extra_images {
			info!(?extra_image, "Pulling extra image to embed");
			cmd_lib::run_cmd!(
				podman pull $extra_image 2>&1;
			)?;
		}

		info!("Current working directory: {}", std::env::current_dir()?.display());
		let digest = cmd_lib::run_fun!(
			podman inspect --format="{{index .Digest}}" $image
		)?
		.trim()
		.to_string();

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

		info!(?d_image, "Creating ephemeral container to extract rootfs");
		std::fs::create_dir_all(chroot)?;
		let digest_trimmed = digest.trim_start_matches("sha256:").get(0..7).unwrap_or("unknown");
		let container_name = format!("katsu-{}", digest_trimmed);

		// Check if container with this name already exists
		let existing_container = cmd_lib::run_fun!(
			podman ps -a --filter name=^${container_name} --format="{{.ID}}"
		)
		.unwrap_or_default();

		let container = if !existing_container.trim().is_empty() {
			info!(?container_name, "Reusing existing container");
			existing_container.trim().to_string()
		} else {
			info!(?container_name, "Creating new container");
			cmd_lib::run_fun!(
				podman create --rm --name ${container_name} $d_image /bin/bash
			)?
			.trim()
			.to_string()
		};

		// experiment: mount container's root fs directly
		let mountpoint = cmd_lib::run_fun!(
			podman mount $container
		)?;
		let mut mountpoint = Path::new(mountpoint.trim()).to_path_buf();
		info!(?mountpoint, "Mountpoint for container's rootfs");

		// XXX: Wonder if we can use skopeo here instead of podman + tar
		let container_store = mountpoint.canonicalize()?.join("var/lib/containers/storage");
		// let container_store_ovfs = container_store.join("overlay");
		std::fs::create_dir_all(&container_store)?;

		// Build list of images to embed
		let mut images_to_embed: Vec<String> = self.embed_extra_images.clone();
		if self.embed_image {
			// Prepend the main image to the list
			images_to_embed.insert(0, image.to_string());
		}

		// Embed all images in the list
		for image_to_embed in &images_to_embed {
			mountpoint = Self::embed_image_to_store(image_to_embed, &container_store, &container)?;
		}

		if self.embed_image_metadata {
			let metadata = BootcImageMetadata { tag: image.to_string(), digest };

			// serialize to yaml
			let serialized = serde_yaml::to_string(&metadata)?;
			tracing::info!(?serialized, "Embedding image metadata into derived image");
			std::fs::write(mountpoint.join(".bootc_meta.yaml"), serialized)?;
		}

		// info!("Exporting container filesystem to chroot");
		// cmd_lib::run_cmd!(
		// 	podman export $container | sudo tar -xf - -C $chroot;
		// )?;

		Ok(TreeOutput::Directory(mountpoint.to_path_buf()))
	}
}
