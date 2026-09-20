use crate::snapshot::KnowledgeSource;
use image::{DynamicImage, ImageFormat, ImageOutputFormat};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub const KEYRING_SERVICE: &str = "kaiwu-clone-fork";
const LEGACY_KEYRING_SERVICE: &str = "buzz-identity-fork";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Persona {
    pub soul: String,
    pub skill: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuzzConfig {
    pub relay_url: String,
    pub home_channel: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub state: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkConfig {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub identity_id: String,
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub avatar_path: Option<PathBuf>,
    #[serde(default)]
    pub additional_instructions: Option<String>,
    #[serde(default)]
    pub additional_constraints: Option<String>,
    pub persona: Persona,
    pub knowledge_sources: Vec<KnowledgeSource>,
    pub model: ModelConfig,
    pub buzz: BuzzConfig,
    #[serde(default)]
    pub runtime: RuntimeState,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSummary {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub identity_id: String,
    pub avatar_path: Option<PathBuf>,
    pub state: String,
    pub has_model_key: bool,
}

const MAX_AVATAR_BYTES: u64 = 2 * 1024 * 1024;
const MAX_AVATAR_DIMENSION: u32 = 4096;
const AVATAR_EDGE: u32 = 512;

pub fn replace_avatar_at(
    root: &Path,
    id: &str,
    current: Option<&Path>,
    source: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let dir = fork_dir(root, id)?;
    let Some(source) = source else {
        remove_managed_avatar(&dir, current)?;
        return Ok(None);
    };
    let metadata = fs::symlink_metadata(source).map_err(|_| "无法读取头像文件。".to_string())?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("头像必须是普通图片文件。".into());
    }
    if metadata.len() > MAX_AVATAR_BYTES {
        return Err("头像不能超过 2 MB。".into());
    }
    let bytes = fs::read(source).map_err(|_| "无法读取头像文件。".to_string())?;
    if bytes.len() as u64 > MAX_AVATAR_BYTES {
        return Err("头像不能超过 2 MB。".into());
    }
    let bytes = normalize_avatar(&bytes)?;
    let target = dir.join("avatar.png");
    let temporary = dir.join(".avatar.tmp");
    fs::write(&temporary, bytes).map_err(|error| format!("无法保存头像：{error}"))?;
    if target.exists() {
        fs::remove_file(&target).map_err(|error| format!("无法更新头像：{error}"))?;
    }
    fs::rename(&temporary, &target).map_err(|error| format!("无法更新头像：{error}"))?;
    #[cfg(unix)]
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444))
        .map_err(|error| format!("无法保护头像文件：{error}"))?;
    remove_managed_avatar(&dir, current.filter(|path| *path != target))?;
    Ok(Some(target))
}

fn normalize_avatar(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let format = image::guess_format(bytes).map_err(|_| "头像仅支持 PNG、JPEG 或 WebP。".to_string())?;
    if !matches!(format, ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP) {
        return Err("头像仅支持 PNG、JPEG 或 WebP。".into());
    }
    let (width, height) = image::io::Reader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| "头像文件已损坏。".to_string())?;
    if width == 0 || height == 0 || width > MAX_AVATAR_DIMENSION || height > MAX_AVATAR_DIMENSION {
        return Err("头像尺寸不能超过 4096×4096。".into());
    }
    let avatar = image::load_from_memory_with_format(bytes, format)
        .map_err(|_| "头像文件已损坏。".to_string())?
        .thumbnail(AVATAR_EDGE, AVATAR_EDGE);
    let avatar = if avatar.color().has_alpha() {
        DynamicImage::ImageRgba8(avatar.to_rgba8())
    } else {
        DynamicImage::ImageRgb8(avatar.to_rgb8())
    };
    let mut output = Cursor::new(Vec::new());
    avatar
        .write_to(&mut output, ImageOutputFormat::Png)
        .map_err(|error| format!("无法规范化头像：{error}"))?;
    Ok(output.into_inner())
}

fn remove_managed_avatar(dir: &Path, current: Option<&Path>) -> Result<(), String> {
    let Some(path) = current else {
        return Ok(());
    };
    let managed = path.parent() == Some(dir)
        && matches!(
            path.file_name().and_then(|name| name.to_str()),
            Some("avatar.png" | "avatar.jpg" | "avatar.webp")
        );
    if managed && path.exists() {
        fs::remove_file(path).map_err(|error| format!("无法删除头像：{error}"))?;
    }
    Ok(())
}

pub fn fork_dir(root: &Path, id: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    Ok(root.join(id))
}

pub fn create_fork_at(root: &Path, config: &ForkConfig) -> Result<(), String> {
    let dir = fork_dir(root, &config.id)?;
    if dir.exists() {
        return Err("分身 ID 已存在。".into());
    }
    fs::create_dir_all(dir.join("config")).map_err(|error| error.to_string())?;
    write_config_at(root, config)
}

pub fn read_fork_at(root: &Path, id: &str) -> Result<ForkConfig, String> {
    let path = fork_dir(root, id)?.join("fork.json");
    let text = fs::read_to_string(&path).map_err(|_| "分身配置不存在。".to_string())?;
    serde_json::from_str(&text).map_err(|_| "分身配置已损坏。".to_string())
}

pub fn write_config_at(root: &Path, config: &ForkConfig) -> Result<(), String> {
    let dir = fork_dir(root, &config.id)?;
    if !dir.exists() {
        return Err("分身不存在。".into());
    }
    let mut text = serde_json::to_string_pretty(config).map_err(|error| error.to_string())?;
    text.push('\n');
    fs::write(dir.join("fork.json"), text).map_err(|error| error.to_string())
}

pub fn list_forks_at(root: &Path) -> Result<Vec<ForkConfig>, String> {
    let mut forks = Vec::new();
    if !root.exists() {
        return Ok(forks);
    }
    let entries = fs::read_dir(root).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if let Ok(config) = read_fork_at(root, &name) {
            forks.push(config);
        }
    }
    forks.sort_by_key(|config| config.created_at);
    Ok(forks)
}

pub fn delete_fork_at(root: &Path, id: &str) -> Result<(), String> {
    let dir = fork_dir(root, id)?;
    if !dir.exists() {
        return Err("分身不存在。".into());
    }
    fs::remove_dir_all(dir).map_err(|error| error.to_string())
}

pub fn write_mount_config(root: &Path, config: &ForkConfig) -> Result<PathBuf, String> {
    let dir = fork_dir(root, &config.id)?;
    let config_dir = dir.join("config");
    fs::create_dir_all(&config_dir).map_err(|error| error.to_string())?;
    fs::write(config_dir.join("SOUL.md"), &config.persona.soul).map_err(|error| error.to_string())?;
    fs::write(config_dir.join("SKILL.md"), &config.persona.skill).map_err(|error| error.to_string())?;
    let mut overlay = serde_json::json!({
        "model": {
            "provider": config.model.provider,
            "default": config.model.model,
        }
    });
    if let Some(base_url) = &config.model.base_url {
        overlay["model"]["base_url"] = serde_json::Value::String(base_url.clone());
    }
    let mut text = serde_json::to_string_pretty(&overlay).map_err(|error| error.to_string())?;
    text.push('\n');
    fs::write(config_dir.join("config.json"), text).map_err(|error| error.to_string())?;
    Ok(config_dir)
}

pub fn model_key_user(id: &str) -> String {
    format!("{}:model", id)
}

pub fn set_model_key(id: &str, key: &str) -> Result<(), String> {
    let entry = model_key_entry(KEYRING_SERVICE, id)?;
    entry.set_password(key).map_err(|error| format!("无法写入系统凭据存储：{}", error))
}

pub fn get_model_key(id: &str) -> Result<String, String> {
    let entry = model_key_entry(KEYRING_SERVICE, id)?;
    match entry.get_password() {
        Ok(key) => Ok(key),
        Err(keyring::Error::NoEntry) => migrate_legacy_model_key(id, &entry),
        Err(_) => Err("该分身尚未设置模型 API Key。".to_string()),
    }
}

pub fn delete_model_key(id: &str) -> Result<(), String> {
    for service in [KEYRING_SERVICE, LEGACY_KEYRING_SERVICE] {
        match model_key_entry(service, id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

pub fn has_model_key(id: &str) -> bool {
    get_model_key(id).is_ok()
}

fn model_key_entry(service: &str, id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, &model_key_user(id)).map_err(|error| error.to_string())
}

fn migrate_legacy_model_key(id: &str, current: &keyring::Entry) -> Result<String, String> {
    let legacy = model_key_entry(LEGACY_KEYRING_SERVICE, id)?;
    let key = legacy
        .get_password()
        .map_err(|_| "该分身尚未设置模型 API Key。".to_string())?;
    current
        .set_password(&key)
        .map_err(|error| format!("无法迁移模型 API Key：{error}"))?;
    match legacy.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(key),
        Err(error) => Err(format!("无法清理旧模型 API Key：{error}")),
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("分身 ID 非法。".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_root(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "fork-store-test-{}-{}-{}",
            name,
            std::process::id(),
            unique
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(id: &str) -> ForkConfig {
        ForkConfig {
            id: id.into(),
            name: "测试分身".into(),
            created_at: 1,
            identity_id: "identity-1".into(),
            domain: Some("测试知识域".into()),
            avatar_path: None,
            additional_instructions: Some("先给结论".into()),
            additional_constraints: Some("不讨论财务".into()),
            persona: Persona {
                soul: "# SOUL\n".into(),
                skill: "# SKILL\n".into(),
            },
            knowledge_sources: vec![KnowledgeSource::Folder {
                label: "docs".into(),
                path: PathBuf::from("/tmp/docs"),
                include: vec![],
            }],
            model: ModelConfig {
                provider: "deepseek".into(),
                model: "deepseek-flash".into(),
                base_url: Some("https://api.deepseek.com/v1".into()),
            },
            buzz: BuzzConfig {
                relay_url: "https://buzz.example".into(),
                home_channel: "chan-1".into(),
            },
            runtime: RuntimeState::default(),
        }
    }

    #[test]
    fn roundtrips_fork_config() {
        let root = temp_root("roundtrip");
        let config = sample("fork-a1");
        create_fork_at(&root, &config).unwrap();
        let loaded = read_fork_at(&root, "fork-a1").unwrap();
        assert_eq!(loaded.name, "测试分身");
        assert_eq!(loaded.domain.as_deref(), Some("测试知识域"));
        assert_eq!(loaded.additional_instructions.as_deref(), Some("先给结论"));
        assert_eq!(loaded.additional_constraints.as_deref(), Some("不讨论财务"));
        assert_eq!(loaded.model.model, "deepseek-flash");
        assert_eq!(list_forks_at(&root).unwrap().len(), 1);
        delete_fork_at(&root, "fork-a1").unwrap();
        assert!(list_forks_at(&root).unwrap().is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reads_legacy_config_without_domain() {
        let root = temp_root("legacy");
        let config = sample("fork-c3");
        create_fork_at(&root, &config).unwrap();
        let path = fork_dir(&root, "fork-c3").unwrap().join("fork.json");
        let mut legacy = serde_json::to_value(config).unwrap();
        legacy.as_object_mut().unwrap().remove("domain");
        legacy.as_object_mut().unwrap().remove("avatarPath");
        legacy.as_object_mut().unwrap().remove("additionalInstructions");
        legacy.as_object_mut().unwrap().remove("additionalConstraints");
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let loaded = read_fork_at(&root, "fork-c3").unwrap();
        assert_eq!(loaded.domain, None);
        assert_eq!(loaded.avatar_path, None);
        assert_eq!(loaded.additional_instructions, None);
        assert_eq!(loaded.additional_constraints, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn stores_and_removes_valid_avatar() {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = u32::MAX;
            for byte in bytes {
                crc ^= u32::from(*byte);
                for _ in 0..8 {
                    crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
                }
            }
            !crc
        }

        let root = temp_root("avatar");
        let config = sample("fork-d4");
        create_fork_at(&root, &config).unwrap();
        let source = root.join("source.png");
        let mut source_bytes = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(1024, 600)
            .write_to(&mut source_bytes, ImageOutputFormat::Png)
            .unwrap();
        let mut source_bytes = source_bytes.into_inner();
        let mut metadata_chunk = Vec::new();
        metadata_chunk.extend_from_slice(&(4u32).to_be_bytes());
        metadata_chunk.extend_from_slice(b"caBX");
        metadata_chunk.extend_from_slice(b"c2pa");
        metadata_chunk.extend_from_slice(&crc32(b"caBXc2pa").to_be_bytes());
        source_bytes.splice(33..33, metadata_chunk);
        assert!(source_bytes.windows(4).any(|chunk| chunk == b"caBX"));
        fs::write(&source, source_bytes).unwrap();

        let avatar = replace_avatar_at(&root, "fork-d4", None, Some(&source)).unwrap();
        assert_eq!(
            avatar.as_ref().and_then(|path| path.extension()),
            Some("png".as_ref())
        );
        assert!(avatar.as_ref().unwrap().is_file());
        assert_eq!(image::image_dimensions(avatar.as_ref().unwrap()).unwrap(), (512, 300));
        assert!(!fs::read(avatar.as_ref().unwrap())
            .unwrap()
            .windows(4)
            .any(|chunk| chunk == b"caBX"));
        assert!(replace_avatar_at(&root, "fork-d4", avatar.as_deref(), None)
            .unwrap()
            .is_none());
        assert!(!avatar.unwrap().exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn writes_mount_config_overlay() {
        let root = temp_root("mount");
        let config = sample("fork-b2");
        create_fork_at(&root, &config).unwrap();
        let dir = write_mount_config(&root, &config).unwrap();
        assert!(dir.join("SOUL.md").exists());
        assert!(dir.join("SKILL.md").exists());
        let overlay: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("config.json")).unwrap()).unwrap();
        assert_eq!(overlay["model"]["provider"], "deepseek");
        assert_eq!(overlay["model"]["base_url"], "https://api.deepseek.com/v1");
        assert!(overlay.get("plugins").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let root = temp_root("traversal");
        assert!(fork_dir(&root, "../escape").is_err());
        assert!(fork_dir(&root, "a/b").is_err());
        assert!(fork_dir(&root, "UPPER").is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
