use crate::{app_data_root, ensure_root, identities_root, read_identity_at};
use fork_core::docker;
use fork_core::snapshot::{self, KnowledgeSource};
use fork_core::store::{self, BuzzConfig, ForkConfig, ForkSummary, ModelConfig, Persona, RuntimeState};
use serde::Deserialize;
use std::path::PathBuf;
use tauri::AppHandle;

const SOUL_TEMPLATE: &str = include_str!("../../runtime/templates/SOUL.md");
const SKILL_TEMPLATE: &str = include_str!("../../runtime/templates/SKILL.md");
const DEFAULT_DOMAIN: &str = "你团队的产品、技术、架构、数据和流程问题";
const DEFAULT_IMAGE: &str = match option_env!("BUZZ_FORK_IMAGE") {
    Some(image) => image,
    None => "buzz-fork-hermes:dev",
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkDraft {
    name: String,
    identity_id: String,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default)]
    avatar_path: Option<PathBuf>,
    #[serde(default)]
    additional_instructions: Option<String>,
    #[serde(default)]
    additional_constraints: Option<String>,
    knowledge_sources: Vec<KnowledgeSource>,
    model: ModelConfig,
    buzz: BuzzConfig,
    model_key: String,
}

fn forks_root(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app_data_root(app)?.join("forks");
    ensure_root(&root)?;
    Ok(root)
}

fn new_fork_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("fork-{}", millis)
}

fn render_persona(
    name: &str,
    domain: Option<&str>,
    additional_instructions: Option<&str>,
    additional_constraints: Option<&str>,
) -> Persona {
    let domain = domain.unwrap_or(DEFAULT_DOMAIN);
    Persona {
        soul: SOUL_TEMPLATE
            .replace("{{FORK_NAME}}", name)
            .replace("{{OWNER_NAME}}", name)
            .replace("{{DOMAIN}}", domain)
            .replace(
                "{{ADDITIONAL_INSTRUCTIONS}}",
                additional_instructions.unwrap_or("无额外设定。"),
            )
            .replace(
                "{{ADDITIONAL_CONSTRAINTS}}",
                additional_constraints.unwrap_or("无额外限制。"),
            ),
        skill: SKILL_TEMPLATE.to_string(),
    }
}

fn normalize_domain(input: Option<String>) -> Option<String> {
    input.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_string())
    })
}

fn normalize_prompt(input: Option<String>, label: &str) -> Result<Option<String>, String> {
    let Some(value) = input else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > 2000
        || value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(format!("{label}最多 2000 个字符，且不能包含控制字符。"));
    }
    Ok(Some(value.to_string()))
}

fn validate_fork_name(input: &str) -> Result<String, String> {
    let name = input.trim();
    if name.is_empty() {
        return Err("分身名称不能为空。".to_string());
    }
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err("分身名称最多 80 个字符，且不能包含控制字符。".to_string());
    }
    Ok(name.to_string())
}

fn container_name(id: &str) -> String {
    format!("buzz-{}", id)
}

fn provider_env_name(provider: &str) -> &'static str {
    match provider {
        "deepseek" => "DEEPSEEK_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        _ => "MODEL_API_KEY",
    }
}

fn runtime_env(config: &ForkConfig) -> Vec<(String, String)> {
    vec![
        ("BUZZ_RELAY_URL".to_string(), config.buzz.relay_url.clone()),
        ("BUZZ_HOME_CHANNEL".to_string(), config.buzz.home_channel.clone()),
        ("BUZZ_ALLOW_ALL_USERS".to_string(), "true".to_string()),
        ("BUZZ_REQUIRE_MENTION".to_string(), "true".to_string()),
        ("BUZZ_TRANSPORT".to_string(), "websocket".to_string()),
        ("BUZZ_CLI_PATH".to_string(), "/usr/local/bin/buzz".to_string()),
        ("HERMES_MODEL".to_string(), config.model.model.clone()),
        ("FORK_PROFILE_NAME".to_string(), config.name.clone()),
        (
            "FORK_PROFILE_ABOUT".to_string(),
            config
                .domain
                .clone()
                .unwrap_or_else(|| DEFAULT_DOMAIN.to_string()),
        ),
    ]
}

fn summary_of(root: &std::path::Path, config: &ForkConfig) -> ForkSummary {
    let status = docker::status(&container_name(&config.id)).unwrap_or(docker::ContainerStatus {
        exists: false,
        running: false,
        started_at: None,
    });
    let state = if status.running {
        "running".to_string()
    } else if status.exists {
        "stopped".to_string()
    } else if config.runtime.snapshot_id.is_some() {
        "ready".to_string()
    } else {
        "draft".to_string()
    };
    let _ = root;
    ForkSummary {
        id: config.id.clone(),
        name: config.name.clone(),
        created_at: config.created_at,
        identity_id: config.identity_id.clone(),
        avatar_path: config.avatar_path.clone(),
        state,
        has_model_key: store::has_model_key(&config.id),
    }
}

#[tauri::command]
pub fn list_forks(app: AppHandle) -> Result<Vec<ForkSummary>, String> {
    let root = forks_root(&app)?;
    let mut summaries: Vec<ForkSummary> = Vec::new();
    for config in store::list_forks_at(&root)? {
        summaries.push(summary_of(&root, &config));
    }
    Ok(summaries)
}

#[tauri::command]
pub fn create_fork(app: AppHandle, draft: ForkDraft) -> Result<ForkConfig, String> {
    let name = validate_fork_name(&draft.name)?;
    if draft.knowledge_sources.is_empty() {
        return Err("至少需要一个知识来源。".to_string());
    }
    if draft.model_key.trim().is_empty() {
        return Err("请填写模型 API Key。".to_string());
    }
    let root = forks_root(&app)?;
    let identities = identities_root(&app)?;
    read_identity_at(&identities, &draft.identity_id)?;

    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let domain = normalize_domain(draft.domain);
    let additional_instructions = normalize_prompt(draft.additional_instructions, "额外设定")?;
    let additional_constraints = normalize_prompt(draft.additional_constraints, "额外限制")?;
    let id = new_fork_id();
    let mut config = ForkConfig {
        id: id.clone(),
        name: name.clone(),
        created_at: millis,
        identity_id: draft.identity_id,
        domain: domain.clone(),
        avatar_path: None,
        additional_instructions: additional_instructions.clone(),
        additional_constraints: additional_constraints.clone(),
        persona: render_persona(
            &name,
            domain.as_deref(),
            additional_instructions.as_deref(),
            additional_constraints.as_deref(),
        ),
        knowledge_sources: draft.knowledge_sources,
        model: draft.model,
        buzz: draft.buzz,
        runtime: RuntimeState::default(),
    };
    store::create_fork_at(&root, &config)?;
    config.avatar_path =
        match store::replace_avatar_at(&root, &id, None, draft.avatar_path.as_deref()) {
            Ok(path) => path,
            Err(error) => {
                let _ = store::delete_fork_at(&root, &id);
                return Err(error);
            }
        };
    store::write_config_at(&root, &config)?;
    store::set_model_key(&config.id, draft.model_key.trim())?;
    Ok(config)
}

#[tauri::command]
pub fn update_fork(app: AppHandle, id: String, draft: ForkDraft) -> Result<ForkConfig, String> {
    let name = validate_fork_name(&draft.name)?;
    if draft.knowledge_sources.is_empty() {
        return Err("至少需要一个知识来源。".to_string());
    }

    let root = forks_root(&app)?;
    let identities = identities_root(&app)?;
    let identity = read_identity_at(&identities, &draft.identity_id)?;

    let mut config = store::read_fork_at(&root, &id)?;
    let knowledge_changed = config.knowledge_sources != draft.knowledge_sources;
    let domain = normalize_domain(draft.domain);
    let additional_instructions = normalize_prompt(draft.additional_instructions, "额外设定")?;
    let additional_constraints = normalize_prompt(draft.additional_constraints, "额外限制")?;
    let avatar_path = store::replace_avatar_at(
        &root,
        &id,
        config.avatar_path.as_deref(),
        draft.avatar_path.as_deref(),
    )?;
    config.name = name.clone();
    config.identity_id = draft.identity_id;
    config.domain = domain.clone();
    config.avatar_path = avatar_path;
    config.additional_instructions = additional_instructions.clone();
    config.additional_constraints = additional_constraints.clone();
    config.persona = render_persona(
        &name,
        domain.as_deref(),
        additional_instructions.as_deref(),
        additional_constraints.as_deref(),
    );
    config.knowledge_sources = draft.knowledge_sources;
    config.model = draft.model;
    config.buzz = draft.buzz;
    if knowledge_changed {
        config.runtime = RuntimeState::default();
    }
    store::write_config_at(&root, &config)?;
    if !draft.model_key.trim().is_empty() {
        store::set_model_key(&id, draft.model_key.trim())?;
    }
    docker::sync_profile(&docker::ProfileSyncSpec {
        image: DEFAULT_IMAGE.to_string(),
        env: runtime_env(&config),
        secret_parent: store::fork_dir(&root, &id)?,
        buzz_private_key: identity.private_key_hex,
        avatar_path: config.avatar_path.clone(),
    })
    .map_err(|error| format!("配置已保存，但 Buzz Profile 同步失败：{error}"))?;
    Ok(config)
}

#[tauri::command]
pub fn get_fork(app: AppHandle, id: String) -> Result<ForkConfig, String> {
    let root = forks_root(&app)?;
    store::read_fork_at(&root, &id)
}

#[tauri::command]
pub fn delete_fork(app: AppHandle, id: String) -> Result<(), String> {
    let root = forks_root(&app)?;
    let _ = docker::remove(&container_name(&id));
    let _ = docker::remove_volume(&format!("{}-state", container_name(&id)));
    store::delete_fork_at(&root, &id)?;
    let _ = store::delete_model_key(&id);
    Ok(())
}

#[tauri::command]
pub fn set_fork_model_key(app: AppHandle, id: String, key: String) -> Result<(), String> {
    let root = forks_root(&app)?;
    store::read_fork_at(&root, &id)?;
    store::set_model_key(&id, key.trim())
}

#[tauri::command]
pub fn build_fork_snapshot(app: AppHandle, id: String) -> Result<snapshot::SnapshotOutcome, String> {
    let root = forks_root(&app)?;
    let mut config = store::read_fork_at(&root, &id)?;
    let fork_dir = store::fork_dir(&root, &id)?;
    let outcome = snapshot::build(&fork_dir, &config.knowledge_sources)?;
    config.runtime.snapshot_id = Some(outcome.snapshot_id.clone());
    config.runtime.state = "ready".to_string();
    store::write_config_at(&root, &config)?;
    Ok(outcome)
}

#[tauri::command]
pub fn docker_probe() -> docker::DockerStatus {
    docker::probe()
}

#[tauri::command]
pub fn start_fork(app: AppHandle, id: String) -> Result<(), String> {
    let root = forks_root(&app)?;
    let mut config = store::read_fork_at(&root, &id)?;
    if let Some(current_avatar) = config.avatar_path.clone() {
        config.avatar_path = store::replace_avatar_at(
            &root,
            &id,
            Some(&current_avatar),
            Some(&current_avatar),
        )?;
        store::write_config_at(&root, &config)?;
    }
    config.persona = render_persona(
        &config.name,
        config.domain.as_deref(),
        config.additional_instructions.as_deref(),
        config.additional_constraints.as_deref(),
    );
    let identities = identities_root(&app)?;
    let identity = read_identity_at(&identities, &config.identity_id)?;
    let model_key = store::get_model_key(&id)?;
    let snapshot_id = config
        .runtime
        .snapshot_id
        .clone()
        .ok_or_else(|| "尚未构建知识快照，请先执行「构建知识」。".to_string())?;

    let fork_dir = store::fork_dir(&root, &id)?;
    let snapshot_dir = fork_dir.join("snapshots").join(&snapshot_id);
    let index_path = fork_dir
        .join("snapshot-receipts")
        .join(&snapshot_id)
        .join("public-index.json");
    if !snapshot_dir.is_dir() || !index_path.is_file() {
        return Err("知识快照缺失，请重新构建。".to_string());
    }

    let config_dir = store::write_mount_config(&root, &config)?;
    let container = container_name(&id);
    let state_volume = format!("{}-state", container);

    let env = runtime_env(&config);

    let spec = docker::RunSpec {
        container_name: container,
        image: DEFAULT_IMAGE.to_string(),
        fork_config_dir: config_dir,
        snapshot_dir,
        index_path,
        state_volume,
        env,
        secret_parent: fork_dir,
        buzz_private_key: identity.private_key_hex,
        model_key_env: provider_env_name(&config.model.provider).to_string(),
        model_key,
        avatar_path: config.avatar_path.clone(),
        command: vec!["gateway".to_string(), "run".to_string()],
    };
    docker::run(&spec)
}

#[tauri::command]
pub fn stop_fork(app: AppHandle, id: String) -> Result<(), String> {
    let root = forks_root(&app)?;
    store::read_fork_at(&root, &id)?;
    docker::stop(&container_name(&id))
}

#[tauri::command]
pub fn fork_status(app: AppHandle, id: String) -> Result<docker::ContainerStatus, String> {
    let root = forks_root(&app)?;
    store::read_fork_at(&root, &id)?;
    docker::status(&container_name(&id))
}

#[tauri::command]
pub fn fork_connection_state(app: AppHandle, id: String) -> Result<docker::ConnectionState, String> {
    let root = forks_root(&app)?;
    store::read_fork_at(&root, &id)?;
    Ok(docker::connection_state(&container_name(&id)))
}

#[tauri::command]
pub fn fork_identity_public_key(app: AppHandle, id: String) -> Result<String, String> {
    let root = forks_root(&app)?;
    let config = store::read_fork_at(&root, &id)?;
    let identities = identities_root(&app)?;
    Ok(read_identity_at(&identities, &config.identity_id)?.public_key_hex)
}

#[tauri::command]
pub fn fork_logs(app: AppHandle, id: String, tail: usize) -> Result<String, String> {
    let root = forks_root(&app)?;
    store::read_fork_at(&root, &id)?;
    docker::logs(&container_name(&id), tail.min(500))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_optional_persona_sections_without_weakening_priority() {
        let persona = render_persona(
            "测试分身",
            Some("测试知识域"),
            Some("先给结论"),
            Some("不讨论财务"),
        );
        assert!(persona.soul.contains("先给结论"));
        assert!(persona.soul.contains("不讨论财务"));
        assert!(persona
            .soul
            .contains("固定安全边界与回答契约 > 额外限制 > 额外设定"));
        assert!(persona
            .soul
            .contains("DM 中对方明确要求记住、更新或删除其长期偏好或约定时"));
        assert!(!persona.soul.contains("{{ADDITIONAL_"));
    }

    #[test]
    fn rejects_oversized_persona_prompt() {
        assert!(normalize_prompt(Some("字".repeat(2001)), "额外设定").is_err());
    }
}
