use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerStatus {
    pub available: bool,
    pub version: Option<String>,
    pub message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerStatus {
    pub exists: bool,
    pub running: bool,
    pub started_at: Option<String>,
}

pub struct RunSpec {
    pub container_name: String,
    pub image: String,
    pub fork_config_dir: PathBuf,
    pub snapshot_dir: PathBuf,
    pub index_path: PathBuf,
    pub state_volume: String,
    pub env: Vec<(String, String)>,
    pub secret_parent: PathBuf,
    pub buzz_private_key: String,
    pub model_key_env: String,
    pub model_key: String,
    pub avatar_path: Option<PathBuf>,
    pub command: Vec<String>,
}

pub struct ProfileSyncSpec {
    pub image: String,
    pub env: Vec<(String, String)>,
    pub secret_parent: PathBuf,
    pub buzz_private_key: String,
    pub avatar_path: Option<PathBuf>,
}

struct SecretDir(PathBuf);

impl Drop for SecretDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn probe() -> DockerStatus {
    match run_docker(&["info", "--format", "{{.ServerVersion}}"]) {
        Ok(version) => DockerStatus {
            available: true,
            version: Some(version.trim().to_string()),
            message: None,
        },
        Err(error) => DockerStatus {
            available: false,
            version: None,
            message: Some(error),
        },
    }
}

pub fn run(spec: &RunSpec) -> Result<(), String> {
    let current = status(&spec.container_name)?;
    if current.running {
        return Err("分身已在运行。".into());
    }
    if current.exists {
        run_docker(&["rm", &spec.container_name])?;
    }

    let secrets = prepare_secrets(spec)?;
    let args = run_args(spec, &secrets.0);
    run_docker_owned(&args)?;
    for _ in 0..300 {
        if run_docker(&[
            "exec",
            &spec.container_name,
            "/bin/sh",
            "-c",
            "test -f /run/fork-secrets-ready",
        ])
        .is_ok()
        {
            return Ok(());
        }
        if !status(&spec.container_name)
            .map(|state| state.running)
            .unwrap_or(false)
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = stop(&spec.container_name);
    Err("分身启动失败或超时，请查看容器日志。".into())
}

pub fn sync_profile(spec: &ProfileSyncSpec) -> Result<(), String> {
    let secrets = prepare_profile_sync_secret(spec)?;
    run_docker_owned(&sync_profile_args(spec, &secrets.0)).map(|_| ())
}

fn run_args(spec: &RunSpec, secret_dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        spec.container_name.clone(),
        "--read-only".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges:true".into(),
        "--pids-limit".into(),
        "256".into(),
        "--user".into(),
        "10000:10000".into(),
        "--entrypoint".into(),
        "/opt/fork/entrypoint.sh".into(),
        "--mount".into(),
        format!("type=volume,src={},dst=/opt/data", spec.state_volume),
        "--mount".into(),
        format!(
            "type=bind,src={},dst=/knowledge,readonly",
            spec.snapshot_dir.display()
        ),
        "--mount".into(),
        format!(
            "type=bind,src={},dst=/run/secrets/fork-public-index,readonly",
            spec.index_path.display()
        ),
        "--mount".into(),
        format!(
            "type=bind,src={},dst=/fork-config,readonly",
            spec.fork_config_dir.display()
        ),
        "--mount".into(),
        format!(
            "type=bind,src={},dst=/opt/fork/secrets/profile.env,readonly",
            secret_dir.join("profile.env").display()
        ),
        "--tmpfs".into(),
        "/run:rw,nosuid,nodev,mode=1777,size=16m".into(),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,noexec,size=128m".into(),
        "-e".into(),
        "HERMES_HOME=/opt/data".into(),
        "-e".into(),
        "HOME=/opt/data".into(),
        "-e".into(),
        "HERMES_ENABLE_PROJECT_PLUGINS=false".into(),
        "-e".into(),
        "FORK_KNOWLEDGE_INDEX=/run/secrets/fork-public-index".into(),
    ];
    for (key, value) in &spec.env {
        args.push("-e".into());
        args.push(format!("{}={}", key, value));
    }
    if let Some(path) = &spec.avatar_path {
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/opt/fork/profile-avatar,readonly",
                path.display()
            ),
        ]);
    }
    args.push(spec.image.clone());
    args.extend(spec.command.iter().cloned());
    args
}

fn sync_profile_args(spec: &ProfileSyncSpec, secret_dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--read-only".into(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges:true".into(),
        "--pids-limit".into(),
        "32".into(),
        "--user".into(),
        "10000:10000".into(),
        "--entrypoint".into(),
        "/bin/sh".into(),
        "--mount".into(),
        format!(
            "type=bind,src={},dst=/opt/fork/secrets/profile.env,readonly",
            secret_dir.join("profile.env").display()
        ),
        "--tmpfs".into(),
        "/tmp:rw,nosuid,nodev,noexec,size=16m".into(),
    ];
    for (key, value) in &spec.env {
        args.push("-e".into());
        args.push(format!("{}={}", key, value));
    }
    if let Some(path) = &spec.avatar_path {
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/opt/fork/profile-avatar,readonly",
                path.display()
            ),
        ]);
    }
    args.push(spec.image.clone());
    args.extend([
        "-ceu".into(),
        "set -a; . /opt/fork/secrets/profile.env; set +a; avatar_url=''; if [ -f /opt/fork/profile-avatar ]; then avatar_result=\"$(/usr/local/bin/buzz upload file --file /opt/fork/profile-avatar)\"; avatar_url=\"$(printf '%s' \"$avatar_result\" | /opt/hermes/.venv/bin/python -c 'import json,sys; print(json.load(sys.stdin)[\"url\"])')\"; fi; exec /usr/local/bin/buzz users set-profile --name \"$FORK_PROFILE_NAME\" --about \"$FORK_PROFILE_ABOUT\" --avatar \"$avatar_url\"".into(),
    ]);
    args
}

fn prepare_secrets(spec: &RunSpec) -> Result<SecretDir, String> {
    prepare_secret_env(
        &spec.secret_parent,
        format!(
            "BUZZ_PRIVATE_KEY={}\n{}={}\n",
            serde_json::to_string(&spec.buzz_private_key).map_err(|error| error.to_string())?,
            spec.model_key_env,
            serde_json::to_string(&spec.model_key).map_err(|error| error.to_string())?,
        ),
    )
}

fn prepare_profile_sync_secret(spec: &ProfileSyncSpec) -> Result<SecretDir, String> {
    prepare_secret_env(
        &spec.secret_parent,
        format!(
            "BUZZ_PRIVATE_KEY={}\n",
            serde_json::to_string(&spec.buzz_private_key).map_err(|error| error.to_string())?,
        ),
    )
}

fn prepare_secret_env(secret_parent: &Path, profile_env: String) -> Result<SecretDir, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let dir = secret_parent.join(format!(".runtime-secrets-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).map_err(|error| format!("无法创建临时凭据目录：{error}"))?;
    let secrets = SecretDir(dir);
    #[cfg(unix)]
    fs::set_permissions(&secrets.0, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("无法保护临时凭据目录：{error}"))?;
    write_secret(&secrets.0.join("profile.env"), profile_env.as_bytes())?;
    Ok(secrets)
}

fn write_secret(path: &Path, value: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| format!("无法创建临时凭据：{error}"))?;
    file.write_all(value)
        .map_err(|error| format!("无法写入临时凭据：{error}"))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o444))
        .map_err(|error| format!("无法设置临时凭据权限：{error}"))?;
    Ok(())
}

pub fn stop(container_name: &str) -> Result<(), String> {
    run_docker(&["stop", container_name]).map(|_| ())
}

pub fn remove(container_name: &str) -> Result<(), String> {
    run_docker(&["rm", "-f", container_name]).map(|_| ())
}

pub fn status(container_name: &str) -> Result<ContainerStatus, String> {
    match run_docker(&[
        "inspect",
        "--format",
        "{{.State.Running}}|{{.State.StartedAt}}",
        container_name,
    ]) {
        Ok(output) => {
            let output = output.trim();
            let mut parts = output.splitn(2, '|');
            let running = parts.next() == Some("true");
            let started_at = parts.next().map(|value| value.to_string());
            Ok(ContainerStatus {
                exists: true,
                running,
                started_at,
            })
        }
        Err(error) => {
            if error.contains("No such") || error.contains("no such") {
                Ok(ContainerStatus {
                    exists: false,
                    running: false,
                    started_at: None,
                })
            } else {
                Err(error)
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionState {
    pub state: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub needs_attention: bool,
}

pub fn connection_state(container_name: &str) -> ConnectionState {
    let idle = |state: &str| ConnectionState {
        state: state.to_string(),
        error_code: None,
        error_message: None,
        needs_attention: false,
    };
    let output = match run_docker(&[
        "exec",
        container_name,
        "cat",
        "/opt/data/profiles/fork/gateway_state.json",
    ]) {
        Ok(output) => output,
        Err(_) => return idle("not_running"),
    };
    let payload: serde_json::Value = match serde_json::from_str(&output) {
        Ok(value) => value,
        Err(_) => return idle("starting"),
    };
    let buzz = &payload["platforms"]["buzz"];
    let state = buzz["state"].as_str().unwrap_or("starting").to_string();
    ConnectionState {
        state,
        error_code: buzz["error_code"].as_str().map(|value| value.to_string()),
        error_message: buzz["error_message"].as_str().map(|value| value.to_string()),
        needs_attention: buzz["needs_attention"].as_bool().unwrap_or(false),
    }
}

pub fn logs(container_name: &str, tail: usize) -> Result<String, String> {
    run_docker(&["logs", "--tail", &tail.to_string(), container_name])
}

pub fn remove_volume(volume: &str) -> Result<(), String> {
    run_docker(&["volume", "rm", volume]).map(|_| ())
}

fn run_docker(args: &[&str]) -> Result<String, String> {
    let output = docker_command()
        .args(args)
        .output()
        .map_err(|error| format!("无法执行 docker，请确认已安装并启动 Docker Desktop：{}", error))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(message);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn run_docker_owned(args: &[String]) -> Result<String, String> {
    let output = docker_command()
        .args(args)
        .output()
        .map_err(|error| format!("无法执行 docker，请确认已安装并启动 Docker Desktop：{}", error))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(message);
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn docker_command() -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new(windows_docker_program(|path| Path::new(path).is_file()));
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        command
    }
    #[cfg(target_os = "macos")]
    {
        Command::new(macos_docker_program(|path| Path::new(path).is_file()))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        Command::new("docker")
    }
}

#[cfg(windows)]
fn windows_docker_program(exists: impl Fn(&str) -> bool) -> &'static str {
    [r"C:\Program Files\Docker\Docker\resources\bin\docker.exe"]
        .into_iter()
        .find(|path| exists(path))
        .unwrap_or("docker")
}

#[cfg(target_os = "macos")]
fn macos_docker_program(exists: impl Fn(&str) -> bool) -> &'static str {
    [
        "/Applications/Docker.app/Contents/Resources/bin/docker",
        "/usr/local/bin/docker",
        "/opt/homebrew/bin/docker",
    ]
    .into_iter()
    .find(|path| exists(path))
    .unwrap_or("docker")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_prefers_docker_desktop_cli_outside_shell_path() {
        assert_eq!(
            windows_docker_program(|path| path.starts_with(r"C:\Program Files\")),
            r"C:\Program Files\Docker\Docker\resources\bin\docker.exe"
        );
        assert_eq!(windows_docker_program(|_| false), "docker");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_prefers_docker_desktop_cli_outside_shell_path() {
        assert_eq!(
            macos_docker_program(|path| path.starts_with("/Applications/")),
            "/Applications/Docker.app/Contents/Resources/bin/docker"
        );
        assert_eq!(macos_docker_program(|_| false), "docker");
    }

    #[test]
    fn docker_args_mount_secrets_without_exposing_values() {
        let parent = std::env::temp_dir().join(format!(
            "fork-docker-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&parent).unwrap();
        let spec = RunSpec {
            container_name: "buzz-fork-test".into(),
            image: "buzz-fork-hermes:dev".into(),
            fork_config_dir: "/tmp/config".into(),
            snapshot_dir: "/tmp/snapshot".into(),
            index_path: "/tmp/index.json".into(),
            state_volume: "buzz-fork-test-state".into(),
            env: vec![
                ("HERMES_MODEL".into(), "deepseek-flash".into()),
                ("FORK_PROFILE_NAME".into(), "测试分身".into()),
                ("FORK_PROFILE_ABOUT".into(), "测试知识域".into()),
            ],
            secret_parent: parent.clone(),
            buzz_private_key: "private-test-value".into(),
            model_key_env: "DEEPSEEK_API_KEY".into(),
            model_key: "model-test-value".into(),
            avatar_path: Some("/tmp/avatar.png".into()),
            command: vec!["gateway".into(), "run".into()],
        };
        let args = run_args(&spec, Path::new("/tmp/fork-test/secrets"));
        let command = args.join(" ");
        assert!(command.contains("/opt/fork/secrets/profile.env"));
        assert!(!command.contains("private-test-value"));
        assert!(!command.contains("model-test-value"));
        assert!(!command.contains("unless-stopped"));
        assert!(command.contains("FORK_PROFILE_NAME=测试分身"));
        assert!(command.contains("FORK_PROFILE_ABOUT=测试知识域"));
        assert!(command.contains(
            "src=/tmp/avatar.png,dst=/opt/fork/profile-avatar,readonly"
        ));

        let secrets = prepare_secrets(&spec).unwrap();
        let secret_path = secrets.0.clone();
        let profile_env = fs::read_to_string(secret_path.join("profile.env")).unwrap();
        assert!(profile_env.contains("BUZZ_PRIVATE_KEY=\"private-test-value\""));
        assert!(profile_env.contains("DEEPSEEK_API_KEY=\"model-test-value\""));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&secret_path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(secrets);
        assert!(!secret_path.exists());

        let sync_spec = ProfileSyncSpec {
            image: "buzz-fork-hermes:dev".into(),
            env: vec![
                ("BUZZ_RELAY_URL".into(), "wss://relay.example".into()),
                ("FORK_PROFILE_NAME".into(), "测试分身".into()),
                ("FORK_PROFILE_ABOUT".into(), "测试知识域".into()),
            ],
            secret_parent: parent.clone(),
            buzz_private_key: "private-test-value".into(),
            avatar_path: Some("/tmp/avatar.png".into()),
        };
        let sync_args = sync_profile_args(&sync_spec, Path::new("/tmp/fork-test/secrets"));
        let sync_command = sync_args.join(" ");
        assert!(sync_command.contains("--rm"));
        assert!(sync_command.contains("--entrypoint /bin/sh"));
        assert!(sync_command.contains("users set-profile"));
        assert!(sync_command.contains("upload file --file /opt/fork/profile-avatar"));
        assert!(sync_command.contains("--avatar \"$avatar_url\""));
        assert!(!sync_command.contains("private-test-value"));

        let sync_secrets = prepare_profile_sync_secret(&sync_spec).unwrap();
        let sync_secret_path = sync_secrets.0.clone();
        let sync_profile_env = fs::read_to_string(sync_secret_path.join("profile.env")).unwrap();
        assert_eq!(sync_profile_env, "BUZZ_PRIVATE_KEY=\"private-test-value\"\n");
        drop(sync_secrets);
        assert!(!sync_secret_path.exists());
        fs::remove_dir(parent).unwrap();
    }
}
