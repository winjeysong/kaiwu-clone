use bech32::{Bech32, Hrp};
use secp256k1::{rand, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use zeroize::Zeroizing;

mod fork_cmd;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const METADATA_FILE: &str = "identity.json";
const PUBLIC_KEY_FILE: &str = "public.key";
const PRIVATE_KEY_FILE: &str = "private.key";
const LEGACY_APP_IDENTIFIER: &str = "com.artpalstudio.buzz-identity";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentitySummary {
    id: String,
    name: String,
    created_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IdentityDetail {
    id: String,
    name: String,
    created_at: u64,
    public_key: String,
    pub(crate) public_key_hex: String,
    private_key: String,
    pub(crate) private_key_hex: String,
}

fn encode_key(prefix: &str, bytes: &[u8]) -> String {
    let hrp = Hrp::parse(prefix).expect("fixed NIP-19 prefix must be valid");
    bech32::encode::<Bech32>(hrp, bytes).expect("32-byte NIP-19 key must encode")
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

fn decode_key_hex(prefix: &str, value: &str) -> Result<String, String> {
    let (hrp, bytes) = bech32::decode(value).map_err(|_| "凭证密钥已损坏。".to_string())?;
    let expected = Hrp::parse(prefix).expect("fixed NIP-19 prefix must be valid");
    if hrp != expected || bytes.len() != 32 {
        return Err("凭证密钥已损坏。".to_string());
    }
    Ok(encode_hex(&bytes))
}

fn validate_name(input: &str) -> Result<String, String> {
    let name = input.trim();
    if name.is_empty() {
        return Err("凭证名称不能为空。".to_string());
    }
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("凭证名称最多 80 个字符，且不能包含控制字符。".to_string());
    }
    Ok(name.to_string())
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 40 || !id.chars().all(|character| character.is_ascii_digit()) {
        return Err("凭证记录无效。".to_string());
    }
    Ok(())
}

pub(crate) fn identities_root(app: &AppHandle) -> Result<PathBuf, String> {
    app_data_root(app).map(|path| path.join("identities"))
}

pub(crate) fn app_data_root(app: &AppHandle) -> Result<PathBuf, String> {
    let current = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法获取应用数据目录：{error}"))?;
    let parent = current.parent().ok_or_else(|| "应用数据目录无效。".to_string())?;
    migrate_legacy_app_data(&current, &parent.join(LEGACY_APP_IDENTIFIER))?;
    Ok(current)
}

fn migrate_legacy_app_data(current: &Path, legacy: &Path) -> Result<(), String> {
    if current.exists() || !legacy.exists() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(legacy).map_err(|error| format!("无法读取旧应用数据：{error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("旧应用数据目录无效。".into());
    }
    fs::rename(legacy, current).map_err(|error| format!("无法迁移旧应用数据：{error}"))
}

pub(crate) fn ensure_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
    #[cfg(unix)]
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("无法设置应用数据目录权限：{error}"))?;
    Ok(())
}

fn write_new_file(path: &Path, value: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    let mut file = options
        .open(path)
        .map_err(|error| format!("无法创建凭证文件：{error}"))?;
    file.write_all(value)
        .map_err(|error| format!("无法写入凭证文件：{error}"))
}

fn timestamp() -> Result<(String, u64), String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "系统时间不可用。".to_string())?;
    Ok((elapsed.as_nanos().to_string(), elapsed.as_millis() as u64))
}

fn create_identity_at(
    root: &Path,
    name: String,
    id: String,
    created_at: u64,
) -> Result<IdentityDetail, String> {
    ensure_root(root)?;
    validate_id(&id)?;
    let name = validate_name(&name)?;
    let target = root.join(&id);
    fs::create_dir(&target).map_err(|error| format!("无法创建凭证目录：{error}"))?;

    #[cfg(unix)]
    if let Err(error) = fs::set_permissions(&target, fs::Permissions::from_mode(0o700)) {
        let _ = fs::remove_dir(&target);
        return Err(format!("无法设置凭证目录权限：{error}"));
    }

    let secp = Secp256k1::new();
    let mut secret_key = SecretKey::new(&mut rand::rng());
    let secret_bytes = Zeroizing::new(secret_key.secret_bytes());
    let (x_only_public_key, _) = secret_key.x_only_public_key(&secp);
    let public_bytes = x_only_public_key.serialize();
    let public_key = encode_key("npub", &public_bytes);
    let public_key_hex = encode_hex(&public_bytes);
    let private_key = Zeroizing::new(encode_key("nsec", secret_bytes.as_ref()));
    let private_key_hex = encode_hex(secret_bytes.as_ref());
    let summary = IdentitySummary {
        id: id.clone(),
        name: name.clone(),
        created_at,
    };

    let write_result = (|| {
        let metadata =
            serde_json::to_vec(&summary).map_err(|error| format!("无法保存凭证信息：{error}"))?;
        write_new_file(&target.join(PRIVATE_KEY_FILE), private_key.as_bytes())?;
        write_new_file(&target.join(PUBLIC_KEY_FILE), public_key.as_bytes())?;
        write_new_file(&target.join(METADATA_FILE), &metadata)?;
        Ok(IdentityDetail {
            id,
            name,
            created_at,
            public_key,
            public_key_hex,
            private_key: private_key.to_string(),
            private_key_hex,
        })
    })();

    secret_key.non_secure_erase();
    if write_result.is_err() {
        let _ = fs::remove_dir_all(&target);
    }
    write_result
}

fn read_summary(target: &Path) -> Result<IdentitySummary, String> {
    let value = fs::read_to_string(target.join(METADATA_FILE))
        .map_err(|error| format!("无法读取凭证信息：{error}"))?;
    serde_json::from_str(&value).map_err(|error| format!("凭证信息已损坏：{error}"))
}

fn list_identities_at(root: &Path) -> Result<Vec<IdentitySummary>, String> {
    ensure_root(root)?;
    let mut identities = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| format!("无法读取凭证列表：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取凭证列表：{error}"))?;
        if entry
            .file_type()
            .map_err(|error| format!("无法读取凭证列表：{error}"))?
            .is_dir()
        {
            let summary = read_summary(&entry.path())?;
            validate_id(&summary.id)?;
            identities.push(summary);
        }
    }
    identities.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(identities)
}

pub(crate) fn read_identity_at(root: &Path, id: &str) -> Result<IdentityDetail, String> {
    validate_id(id)?;
    let target = root.join(id);
    let summary = read_summary(&target)?;
    if summary.id != id {
        return Err("凭证记录已损坏。".to_string());
    }
    let public_key = fs::read_to_string(target.join(PUBLIC_KEY_FILE))
        .map_err(|error| format!("无法读取公钥：{error}"))?;
    let private_key = fs::read_to_string(target.join(PRIVATE_KEY_FILE))
        .map_err(|error| format!("无法读取私钥：{error}"))?;
    let public_key_hex = decode_key_hex("npub", &public_key)?;
    let private_key_hex = decode_key_hex("nsec", &private_key)?;
    Ok(IdentityDetail {
        id: summary.id,
        name: summary.name,
        created_at: summary.created_at,
        public_key,
        public_key_hex,
        private_key,
        private_key_hex,
    })
}

fn rename_identity_at(root: &Path, id: &str, name: &str) -> Result<IdentitySummary, String> {
    validate_id(id)?;
    let target = root.join(id);
    let mut summary = read_summary(&target)?;
    if summary.id != id {
        return Err("凭证记录已损坏。".to_string());
    }
    summary.name = validate_name(name)?;
    let metadata =
        serde_json::to_vec(&summary).map_err(|error| format!("无法保存凭证信息：{error}"))?;
    fs::write(target.join(METADATA_FILE), metadata)
        .map_err(|error| format!("无法保存凭证名称：{error}"))?;
    Ok(summary)
}

fn delete_identity_at(root: &Path, id: &str) -> Result<(), String> {
    validate_id(id)?;
    let target = root.join(id);
    if !target.is_dir() {
        return Err("凭证记录不存在。".to_string());
    }
    fs::remove_dir_all(target).map_err(|error| format!("无法删除凭证记录：{error}"))
}

#[tauri::command]
fn list_identities(app: AppHandle) -> Result<Vec<IdentitySummary>, String> {
    list_identities_at(&identities_root(&app)?)
}

#[tauri::command]
fn generate_identity(app: AppHandle, name: String) -> Result<IdentityDetail, String> {
    let (id, created_at) = timestamp()?;
    create_identity_at(&identities_root(&app)?, name, id, created_at)
}

#[tauri::command]
fn get_identity(app: AppHandle, id: String) -> Result<IdentityDetail, String> {
    read_identity_at(&identities_root(&app)?, &id)
}

#[tauri::command]
fn rename_identity(app: AppHandle, id: String, name: String) -> Result<IdentitySummary, String> {
    rename_identity_at(&identities_root(&app)?, &id, &name)
}

#[tauri::command]
fn delete_identity(app: AppHandle, id: String) -> Result<(), String> {
    delete_identity_at(&identities_root(&app)?, &id)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            list_identities,
            generate_identity,
            get_identity,
            rename_identity,
            delete_identity,
            fork_cmd::list_forks,
            fork_cmd::create_fork,
            fork_cmd::update_fork,
            fork_cmd::get_fork,
            fork_cmd::delete_fork,
            fork_cmd::set_fork_model_key,
            fork_cmd::build_fork_snapshot,
            fork_cmd::docker_probe,
            fork_cmd::start_fork,
            fork_cmd::stop_fork,
            fork_cmd::fork_status,
            fork_cmd::fork_logs,
            fork_cmd::fork_connection_state,
            fork_cmd::fork_identity_public_key
        ])
        .run(tauri::generate_context!())
        .expect("error while running Kaiwu Clone");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_nip19_vectors() {
        let public = hex("7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e");
        let private = hex("67dea2ed018072d675f5415ecfaed7d2597555e202d85b3d65ea4e58d2d92ffa");
        assert_eq!(
            encode_key("npub", &public),
            "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg"
        );
        assert_eq!(
            encode_key("nsec", &private),
            "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5"
        );
    }

    #[test]
    fn manages_identities_in_app_storage() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kaiwu-clone-test-{unique}"));

        let first =
            create_identity_at(&root, "测试账号".to_string(), "1001".to_string(), 1001).unwrap();
        let second = create_identity_at(
            &root,
            "2026-09-14 18:00:00".to_string(),
            "1002".to_string(),
            1002,
        )
        .unwrap();
        assert!(first.public_key.starts_with("npub1"));
        assert!(first.private_key.starts_with("nsec1"));
        let first_json = serde_json::to_value(&first).unwrap();
        for field in ["publicKeyHex", "privateKeyHex"] {
            let value = first_json[field].as_str().expect("hex key must be present");
            assert_eq!(value.len(), 64);
            assert!(value
                .bytes()
                .all(|character| character.is_ascii_digit() || (b'a'..=b'f').contains(&character)));
        }
        assert_eq!(list_identities_at(&root).unwrap()[0].id, second.id);

        let renamed = rename_identity_at(&root, &first.id, "同事 A").unwrap();
        assert_eq!(renamed.name, "同事 A");
        let loaded = read_identity_at(&root, &first.id).unwrap();
        assert_eq!(loaded.name, "同事 A");
        let loaded_json = serde_json::to_value(loaded).unwrap();
        assert_eq!(loaded_json["publicKeyHex"], first_json["publicKeyHex"]);
        assert_eq!(loaded_json["privateKeyHex"], first_json["privateKeyHex"]);

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(root.join(&first.id).join(PRIVATE_KEY_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        delete_identity_at(&root, &first.id).unwrap();
        assert_eq!(list_identities_at(&root).unwrap().len(), 1);
        assert!(validate_name("  ").is_err());
        assert!(read_identity_at(&root, "../secret").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn migrates_legacy_app_data_once() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("kaiwu-clone-migration-test-{unique}"));
        let legacy = root.join(LEGACY_APP_IDENTIFIER);
        let current = root.join("com.artpalstudio.kaiwu-clone");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("identity.json"), "legacy").unwrap();

        migrate_legacy_app_data(&current, &legacy).unwrap();
        assert!(!legacy.exists());
        assert_eq!(fs::read_to_string(current.join("identity.json")).unwrap(), "legacy");
        migrate_legacy_app_data(&current, &legacy).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text = std::str::from_utf8(pair).unwrap();
                u8::from_str_radix(text, 16).unwrap()
            })
            .collect()
    }
}
