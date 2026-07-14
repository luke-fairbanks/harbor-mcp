use serde::Deserialize;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const MAX_DESCRIPTOR_BYTES: u64 = 64 * 1024;
const MIN_TOKEN_BYTES: usize = 16;
const MAX_TOKEN_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Descriptor {
    pub schema_version: u32,
    pub instance_id: String,
    pub pid: u32,
    pub process_started_at: Option<String>,
    pub port: u16,
    pub token: String,
    pub app_executable: Option<String>,
}

#[derive(Debug)]
pub(crate) enum DescriptorError {
    Missing,
    Unsafe,
    TooLarge,
    Invalid,
    Io,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDescriptor {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    instance_id: String,
    #[serde(default)]
    pid: u32,
    #[serde(default)]
    process_started_at: Option<String>,
    port: u16,
    token: String,
    #[serde(default)]
    app_executable: Option<String>,
}

fn default_schema_version() -> u32 {
    1
}

pub(crate) fn read_descriptor(path: &Path) -> Result<Descriptor, DescriptorError> {
    let parent = path.parent().ok_or(DescriptorError::Unsafe)?;
    let parent_metadata = std::fs::symlink_metadata(parent).map_err(map_open_error)?;
    let effective_uid = unsafe { libc::geteuid() };
    if !parent_metadata.file_type().is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != effective_uid
        || parent_metadata.mode() & 0o077 != 0
    {
        return Err(DescriptorError::Unsafe);
    }

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(map_open_error)?;
    read_open_descriptor(file)
}

fn map_open_error(error: io::Error) -> DescriptorError {
    if error.kind() == io::ErrorKind::NotFound {
        DescriptorError::Missing
    } else if error.raw_os_error() == Some(libc::ELOOP) {
        DescriptorError::Unsafe
    } else {
        DescriptorError::Io
    }
}

fn read_open_descriptor(file: File) -> Result<Descriptor, DescriptorError> {
    let metadata = file.metadata().map_err(|_| DescriptorError::Io)?;
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(DescriptorError::Unsafe);
    }
    if metadata.len() == 0 || metadata.len() > MAX_DESCRIPTOR_BYTES {
        return Err(DescriptorError::TooLarge);
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DescriptorError::Io)?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(DescriptorError::TooLarge);
    }
    let raw: RawDescriptor =
        serde_json::from_slice(&bytes).map_err(|_| DescriptorError::Invalid)?;
    validate(raw)
}

fn validate(raw: RawDescriptor) -> Result<Descriptor, DescriptorError> {
    if raw.schema_version != 1 || raw.port == 0 || !valid_token(&raw.token) {
        return Err(DescriptorError::Invalid);
    }
    if !valid_optional_text(&raw.instance_id, 256)
        || raw
            .process_started_at
            .as_deref()
            .is_some_and(|value| !valid_optional_text(value, 128))
        || raw
            .app_executable
            .as_deref()
            .is_some_and(|value| !valid_executable_text(value))
    {
        return Err(DescriptorError::Invalid);
    }

    Ok(Descriptor {
        schema_version: raw.schema_version,
        instance_id: raw.instance_id,
        pid: raw.pid,
        process_started_at: raw.process_started_at,
        port: raw.port,
        token: raw.token,
        app_executable: raw.app_executable,
    })
}

fn valid_token(token: &str) -> bool {
    (MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

fn valid_optional_text(value: &str, max: usize) -> bool {
    value.len() <= max && value.chars().all(|ch| !ch.is_control())
}

fn valid_executable_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && Path::new(value).is_absolute()
        && value.chars().all(|ch| !ch.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};

    fn private_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("mcp.json");
        let mut file = File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        (dir, path)
    }

    #[test]
    fn accepts_current_and_legacy_descriptors() {
        let (_dir, path) = private_file(
            r#"{"schemaVersion":1,"instanceId":"one","pid":7,"processStartedAt":"now","port":4312,"token":"0123456789abcdef","appExecutable":"/Applications/Harbor.app/Contents/MacOS/Harbor"}"#,
        );
        let descriptor = read_descriptor(&path).unwrap();
        assert_eq!(descriptor.port, 4312);
        assert_eq!(descriptor.instance_id, "one");

        let (_legacy_dir, legacy_path) =
            private_file(r#"{"port":4313,"token":"fedcba9876543210"}"#);
        let legacy = read_descriptor(&legacy_path).unwrap();
        assert_eq!(legacy.schema_version, 1);
        assert_eq!(legacy.pid, 0);
    }

    #[test]
    fn rejects_malformed_tokens_and_permissions() {
        for token in ["short", "0123456789abcde\n", "0123456789abcde\\"] {
            let (_dir, path) = private_file(&format!(
                "{{\"port\":4312,\"token\":{}}}",
                serde_json::to_string(token).unwrap()
            ));
            assert!(matches!(
                read_descriptor(&path),
                Err(DescriptorError::Invalid)
            ));
        }

        let (_dir, path) = private_file(r#"{"port":4312,"token":"0123456789abcdef"}"#);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            read_descriptor(&path),
            Err(DescriptorError::Unsafe)
        ));
    }

    #[test]
    fn refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let target = dir.path().join("real.json");
        std::fs::write(&target, r#"{"port":4312,"token":"0123456789abcdef"}"#).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("mcp.json");
        symlink(&target, &link).unwrap();
        assert!(matches!(
            read_descriptor(&link),
            Err(DescriptorError::Unsafe)
        ));
    }

    #[test]
    fn requires_schema_one_private_parent_and_single_link() {
        let (_schema_dir, schema_path) =
            private_file(r#"{"schemaVersion":2,"port":4312,"token":"0123456789abcdef"}"#);
        assert!(matches!(
            read_descriptor(&schema_path),
            Err(DescriptorError::Invalid)
        ));

        let (parent_dir, parent_path) = private_file(r#"{"port":4312,"token":"0123456789abcdef"}"#);
        std::fs::set_permissions(parent_dir.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(matches!(
            read_descriptor(&parent_path),
            Err(DescriptorError::Unsafe)
        ));

        let (link_dir, link_path) = private_file(r#"{"port":4312,"token":"0123456789abcdef"}"#);
        std::fs::hard_link(&link_path, link_dir.path().join("second-link.json")).unwrap();
        assert!(matches!(
            read_descriptor(&link_path),
            Err(DescriptorError::Unsafe)
        ));
    }
}
