use std::path::{Path, PathBuf};

use base64::Engine as _;
use sha2::{Digest as ShaDigest, Sha256};

use super::{ContainerError, ContainerId, ContainerRuntime, Result};

#[derive(Debug)]
pub struct LayerBlob {
    pub path: PathBuf,
    pub digest: String,
    pub size: u64,
    pub diff_id: String,
}

#[derive(Debug, Clone)]
pub struct ImageReference {
    pub registry: String,
    pub repository: String,
    pub reference: String,
}

impl ImageReference {
    pub fn parse(image_ref: &str) -> Result<Self> {
        if let Some((repo_part, digest)) = image_ref.split_once('@') {
            let (registry, repository) = split_registry_repo(repo_part);
            return Ok(Self {
                registry,
                repository,
                reference: digest.to_string(),
            });
        }

        let (repo_part, tag) = if let Some(slash_pos) = image_ref.find('/') {
            let after_slash = &image_ref[slash_pos..];
            if let Some(colon_pos) = after_slash.rfind(':') {
                let tag = &after_slash[colon_pos + 1..];
                let repo = &image_ref[..slash_pos + colon_pos];
                (repo.to_string(), tag.to_string())
            } else {
                (image_ref.to_string(), "latest".to_string())
            }
        } else if let Some((name, tag)) = image_ref.split_once(':') {
            (name.to_string(), tag.to_string())
        } else {
            (image_ref.to_string(), "latest".to_string())
        };

        let (registry, repository) = split_registry_repo(&repo_part);
        Ok(Self {
            registry,
            repository,
            reference: tag,
        })
    }

    pub fn full_ref(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.reference)
    }
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}/{}:{}",
            self.registry, self.repository, self.reference
        )
    }
}

fn split_registry_repo(repo_part: &str) -> (String, String) {
    if let Some(slash_pos) = repo_part.find('/') {
        let first = &repo_part[..slash_pos];
        if first.contains('.') || first.contains(':') {
            return (
                first.to_string(),
                repo_part[slash_pos + 1..].to_string(),
            );
        }
    }

    if repo_part.contains('/') {
        ("docker.io".to_string(), repo_part.to_string())
    } else {
        ("docker.io".to_string(), format!("library/{}", repo_part))
    }
}

pub fn create_layer(
    source_dir: &Path,
    container_prefix: &str,
    output_dir: &Path,
) -> Result<LayerBlob> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::fs;
    use std::io::Write;

    let mut tar_data = Vec::new();
    {
        let mut tar_builder = tar::Builder::new(&mut tar_data);
        tar_builder
            .append_dir_all(container_prefix, source_dir)
            .map_err(|e| {
                ContainerError::OciError(format!(
                    "failed to create tar from {}: {}",
                    source_dir.display(),
                    e
                ))
            })?;
        tar_builder.finish().map_err(|e| {
            ContainerError::OciError(format!("failed to finalize tar: {}", e))
        })?;
    }

    let diff_id = {
        let mut hasher = Sha256::new();
        hasher.update(&tar_data);
        let hash = hasher.finalize();
        format!("sha256:{}", hex::encode(hash))
    };

    let compressed_data = {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar_data).map_err(|e| {
            ContainerError::OciError(format!("gzip compression failed: {}", e))
        })?;
        encoder.finish().map_err(|e| {
            ContainerError::OciError(format!("gzip finalization failed: {}", e))
        })?
    };

    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&compressed_data);
        let hash = hasher.finalize();
        format!("sha256:{}", hex::encode(hash))
    };

    let size = compressed_data.len() as u64;

    fs::create_dir_all(output_dir)?;
    let filename = format!("{}.tar.gz", &digest[7..]);
    let blob_path = output_dir.join(&filename);
    fs::write(&blob_path, &compressed_data)?;

    tracing::info!(
        digest = %digest,
        diff_id = %diff_id,
        size = size,
        path = %blob_path.display(),
        "OCI layer created"
    );

    Ok(LayerBlob {
        path: blob_path,
        digest,
        size,
        diff_id,
    })
}

pub async fn extract_and_build_layer(
    runtime: &dyn ContainerRuntime,
    container_id: &ContainerId,
    container_src: &str,
    container_prefix: &str,
    work_dir: &Path,
    output_dir: &Path,
) -> Result<LayerBlob> {
    let extract_dir = work_dir.join("extract");
    std::fs::create_dir_all(&extract_dir)?;

    runtime
        .cp(container_id, container_src, &extract_dir)
        .await?;

    let basename = Path::new(container_src)
        .file_name()
        .ok_or_else(|| {
            ContainerError::OciError(format!(
                "cannot determine basename of container path: {}",
                container_src
            ))
        })?;
    let source_path = extract_dir.join(basename);

    if !source_path.exists() {
        return Err(ContainerError::OciError(format!(
            "extracted path does not exist: {}",
            source_path.display()
        )));
    }

    let container_prefix = container_prefix.to_string();
    let output_dir = output_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        create_layer(&source_path, &container_prefix, &output_dir)
    })
    .await
    .expect("blocking task panicked")
}

pub async fn push_image_with_layer(
    base_ref: &str,
    target_ref: &str,
    layer: &LayerBlob,
) -> Result<()> {
    use oci_client::manifest::{
        OciDescriptor, IMAGE_LAYER_GZIP_MEDIA_TYPE,
        IMAGE_CONFIG_MEDIA_TYPE,
    };
    use oci_client::{Client, Reference};

    let base: Reference = base_ref.parse().map_err(|e: oci_client::ParseError| {
        ContainerError::OciError(format!("invalid base reference '{}': {}", base_ref, e))
    })?;
    let target: Reference =
        target_ref
            .parse()
            .map_err(|e: oci_client::ParseError| {
                ContainerError::OciError(format!(
                    "invalid target reference '{}': {}",
                    target_ref, e
                ))
            })?;

    let client_config = oci_client::client::ClientConfig {
        protocol: oci_client::client::ClientProtocol::Https,
        ..Default::default()
    };
    let client = Client::new(client_config);

    let auth = resolve_registry_auth(base.resolve_registry());

    client
        .auth(&base, &auth, oci_client::RegistryOperation::Pull)
        .await
        .map_err(|e| {
            ContainerError::OciError(format!("auth for pull failed: {}", e))
        })?;

    let (mut manifest, _manifest_digest, config_str) = client
        .pull_manifest_and_config(&base, &auth)
        .await
        .map_err(|e| {
            ContainerError::OciError(format!("failed to pull base manifest: {}", e))
        })?;

    let mut config: serde_json::Value =
        serde_json::from_str(&config_str).map_err(|e| {
            ContainerError::OciError(format!("config deserialization error: {}", e))
        })?;

    let layer_data = std::fs::read(&layer.path)?;

    client
        .auth(
            &target,
            &auth,
            oci_client::RegistryOperation::Push,
        )
        .await
        .map_err(|e| {
            ContainerError::OciError(format!("auth for push failed: {}", e))
        })?;

    client
        .push_blob(&target, &layer_data, &layer.digest)
        .await
        .map_err(|e| {
            ContainerError::OciError(format!("failed to push layer blob: {}", e))
        })?;

    let layer_descriptor = OciDescriptor {
        media_type: IMAGE_LAYER_GZIP_MEDIA_TYPE.to_string(),
        digest: layer.digest.clone(),
        size: layer.size as i64,
        urls: None,
        annotations: None,
    };
    manifest.layers.push(layer_descriptor);

    if let Some(rootfs) = config.get_mut("rootfs") {
        if let Some(diff_ids) = rootfs.get_mut("diff_ids") {
            if let Some(arr) = diff_ids.as_array_mut() {
                arr.push(serde_json::Value::String(layer.diff_id.clone()));
            }
        }
    }

    let config_json = serde_json::to_string(&config).map_err(|e| {
        ContainerError::OciError(format!("config serialization error: {}", e))
    })?;
    let config_bytes = config_json.as_bytes();
    let config_digest = {
        let mut hasher = Sha256::new();
        hasher.update(config_bytes);
        format!("sha256:{}", hex::encode(hasher.finalize()))
    };

    client
        .push_blob(&target, config_bytes, &config_digest)
        .await
        .map_err(|e| {
            ContainerError::OciError(format!("failed to push config blob: {}", e))
        })?;

    manifest.config = OciDescriptor {
        media_type: IMAGE_CONFIG_MEDIA_TYPE.to_string(),
        digest: config_digest,
        size: config_bytes.len() as i64,
        urls: None,
        annotations: None,
    };

    let oci_manifest = oci_client::manifest::OciManifest::Image(manifest);
    client
        .push_manifest(&target, &oci_manifest)
        .await
        .map_err(|e| {
            ContainerError::OciError(format!("failed to push manifest: {}", e))
        })?;

    tracing::info!(
        base = %base_ref,
        target = %target_ref,
        layer_digest = %layer.digest,
        layer_size = layer.size,
        "image pushed with appended layer"
    );

    Ok(())
}

fn resolve_registry_auth(registry: &str) -> oci_client::secrets::RegistryAuth {
    // Try reading Docker config.json for credential lookup
    let config_path = dirs::home_dir()
        .map(|h| h.join(".docker").join("config.json"))
        .filter(|p| p.exists());

    if let Some(path) = config_path {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&contents) {
                // Try direct auth lookup in "auths" section
                if let Some(auths) = config.get("auths").and_then(|a| a.as_object()) {
                    // Docker Hub uses both "https://index.docker.io/v1/" and "docker.io"
                    let registry_keys = [
                        registry.to_string(),
                        format!("https://{}", registry),
                        format!("https://{}/v1/", registry),
                    ];

                    for key in &registry_keys {
                        if let Some(entry) = auths.get(key.as_str()) {
                            if let Some(auth_b64) = entry.get("auth").and_then(|a| a.as_str()) {
                                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(auth_b64) {
                                    if let Ok(decoded_str) = String::from_utf8(decoded) {
                                        if let Some((user, pass)) = decoded_str.split_once(':') {
                                            return oci_client::secrets::RegistryAuth::Basic(
                                                user.to_string(),
                                                pass.to_string(),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Fall back to environment variables
    if let (Ok(user), Ok(pass)) = (
        std::env::var("REGISTRY_USERNAME"),
        std::env::var("REGISTRY_PASSWORD"),
    ) {
        return oci_client::secrets::RegistryAuth::Basic(user, pass);
    }

    tracing::debug!(
        registry = registry,
        "No credentials found for registry; using anonymous auth"
    );
    oci_client::secrets::RegistryAuth::Anonymous
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_reference() {
        let r = ImageReference::parse("ghcr.io/myorg/bot:v1").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "myorg/bot");
        assert_eq!(r.reference, "v1");
    }

    #[test]
    fn parse_no_tag_defaults_to_latest() {
        let r = ImageReference::parse("ghcr.io/myorg/bot").unwrap();
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_bare_name() {
        let r = ImageReference::parse("nginx").unwrap();
        assert_eq!(r.registry, "docker.io");
        assert_eq!(r.repository, "library/nginx");
        assert_eq!(r.reference, "latest");
    }

    #[test]
    fn parse_docker_hub_user_repo() {
        let r = ImageReference::parse("myuser/myrepo:1.0").unwrap();
        assert_eq!(r.registry, "docker.io");
        assert_eq!(r.repository, "myuser/myrepo");
        assert_eq!(r.reference, "1.0");
    }

    #[test]
    fn parse_digest_reference() {
        let r =
            ImageReference::parse("ghcr.io/myorg/bot@sha256:abc123def456").unwrap();
        assert_eq!(r.registry, "ghcr.io");
        assert_eq!(r.repository, "myorg/bot");
        assert_eq!(r.reference, "sha256:abc123def456");
    }

    #[test]
    fn parse_registry_with_port() {
        let r = ImageReference::parse("localhost:5000/myimage:dev").unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.repository, "myimage");
        assert_eq!(r.reference, "dev");
    }
}
