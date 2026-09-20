use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RICH_FILE_BYTES: usize = 16_000_000;
const DEFAULT_DENY_PREFIXES: &[&str] = &[
    ".git/", ".m2/", ".pnpm-store/", "backup/", "backups/", "build/", "dist/", "logs/", "node_modules/",
    "target/", "temp/", "tmp/",
];
const DEFAULT_DENY_SUFFIXES: &[&str] = &[".key", ".pem", ".p12", ".pfx"];
const DEFAULT_DENY_NAMES: &[&str] = &[".env", ".env.local", ".env.development", ".env.production"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum KnowledgeSource {
    Git {
        label: String,
        repo_path: PathBuf,
        commit: String,
        #[serde(default)]
        include: Vec<String>,
        #[serde(default)]
        exclude: Vec<String>,
    },
    Folder {
        label: String,
        path: PathBuf,
        #[serde(default)]
        include: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotOutcome {
    pub snapshot_id: String,
    pub file_count: usize,
    pub snapshot_dir: PathBuf,
    pub index_path: PathBuf,
}

struct CollectedFile {
    path: String,
    bytes: Vec<u8>,
    source_type: &'static str,
    source_path: String,
    repository: Option<String>,
    commit: Option<String>,
}

struct SourceRecord {
    label: String,
    source_type: &'static str,
    reference: String,
}

pub fn build(out_root: &Path, sources: &[KnowledgeSource]) -> Result<SnapshotOutcome, String> {
    if sources.is_empty() {
        return Err("至少需要一个知识来源。".into());
    }

    let mut files: BTreeMap<String, CollectedFile> = BTreeMap::new();
    let mut records = Vec::new();
    let mut total_bytes: u64 = 0;

    for source in sources {
        let (collected, record) = match source {
            KnowledgeSource::Git { label, repo_path, commit, include, exclude } => {
                collect_git(label, repo_path, commit, include, exclude)?
            }
            KnowledgeSource::Folder { label, path, include } => collect_folder(label, path, include)?,
        };
        records.push(record);
        for file in collected {
            if is_rich_file(&file.path) && file.bytes.len() > MAX_RICH_FILE_BYTES {
                return Err(format!("{} 超过单个 PDF、Office 或图片文件的 16MB 上限。", file.path));
            }
            if let Some(existing) = files.get(&file.path) {
                let _ = existing;
                return Err(format!("多个知识来源产生相同路径：{}", file.path));
            }
            total_bytes += file.bytes.len() as u64;
            if total_bytes > MAX_TOTAL_BYTES {
                return Err("知识快照超过 64MB 上限，请缩小来源范围。".into());
            }
            files.insert(file.path.clone(), file);
        }
    }

    if files.is_empty() {
        return Err("知识来源没有匹配到任何文件。".into());
    }

    let digest_input: Vec<serde_json::Value> = records
        .iter()
        .map(|record| {
            serde_json::json!({
                "label": record.label,
                "type": record.source_type,
                "reference": record.reference,
            })
        })
        .collect();
    let digest_source = serde_json::to_string(&digest_input).map_err(|error| error.to_string())?;
    let digest = sha256_hex(digest_source.as_bytes());
    let snapshot_id = format!("{}-{}", utc_timestamp(), &digest[..12]);

    let snapshots_root = out_root.join("snapshots");
    let receipts_root = out_root.join("snapshot-receipts");
    fs::create_dir_all(&snapshots_root).map_err(|error| error.to_string())?;
    fs::create_dir_all(&receipts_root).map_err(|error| error.to_string())?;

    let snapshot_dir = snapshots_root.join(&snapshot_id);
    let receipt_dir = receipts_root.join(&snapshot_id);
    if snapshot_dir.exists() || receipt_dir.exists() {
        return Err("快照 ID 已存在，不能覆盖。".into());
    }

    let snapshot_tmp = snapshots_root.join(format!(".{}.tmp", snapshot_id));
    let receipt_tmp = receipts_root.join(format!(".{}.tmp", snapshot_id));
    let write = || -> Result<(), String> {
        fs::create_dir_all(&snapshot_tmp).map_err(|error| error.to_string())?;
        fs::create_dir_all(&receipt_tmp).map_err(|error| error.to_string())?;

        let mut manifest_files = Vec::new();
        for (path, file) in &files {
            let target = snapshot_tmp.join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::write(&target, &file.bytes).map_err(|error| error.to_string())?;
            let mut entry = serde_json::json!({
                "path": path,
                "sha256": sha256_hex(&file.bytes),
                "size_bytes": file.bytes.len(),
                "source_path": file.source_path,
                "source_type": file.source_type,
                "visibility": "team",
            });
            if let Some(repository) = &file.repository {
                entry["repository"] = serde_json::Value::String(repository.clone());
            }
            if let Some(commit) = &file.commit {
                entry["commit"] = serde_json::Value::String(commit.clone());
            }
            manifest_files.push(entry);
        }

        let manifest = serde_json::json!({
            "schema_version": 1,
            "snapshot_id": snapshot_id,
            "generated_at_utc": utc_iso(),
            "visibility": "team",
            "files": manifest_files,
        });
        write_json(&receipt_tmp.join("MANIFEST.json"), &manifest)?;

        let index = serde_json::json!({
            "schema_version": 1,
            "snapshot_id": snapshot_id,
            "files": manifest["files"].clone(),
        });
        write_json(&receipt_tmp.join("public-index.json"), &index)?;

        let records_json: Vec<serde_json::Value> = records
            .iter()
            .map(|record| {
                serde_json::json!({
                    "label": record.label,
                    "type": record.source_type,
                    "reference": record.reference,
                })
            })
            .collect();
        let sources_json = serde_json::json!({
            "schema_version": 1,
            "snapshot_id": snapshot_id,
            "generated_at_utc": utc_iso(),
            "sources": records_json,
        });
        write_json(&receipt_tmp.join("sources.json"), &sources_json)?;
        Ok(())
    };

    if let Err(error) = write() {
        let _ = fs::remove_dir_all(&snapshot_tmp);
        let _ = fs::remove_dir_all(&receipt_tmp);
        return Err(error);
    }

    if let Err(error) = fs::rename(&snapshot_tmp, &snapshot_dir) {
        let _ = fs::remove_dir_all(&snapshot_tmp);
        let _ = fs::remove_dir_all(&receipt_tmp);
        return Err(error.to_string());
    }
    if let Err(error) = fs::rename(&receipt_tmp, &receipt_dir) {
        let _ = fs::remove_dir_all(&snapshot_dir);
        let _ = fs::remove_dir_all(&receipt_tmp);
        return Err(error.to_string());
    }
    lock_read_only(&snapshot_dir);
    lock_read_only(&receipt_dir);

    Ok(SnapshotOutcome {
        snapshot_id,
        file_count: files.len(),
        snapshot_dir: snapshot_dir.clone(),
        index_path: receipt_dir.join("public-index.json"),
    })
}

fn collect_git(
    label: &str,
    repo_path: &Path,
    commit: &str,
    include: &[String],
    exclude: &[String],
) -> Result<(Vec<CollectedFile>, SourceRecord), String> {
    if !repo_path.join(".git").exists() {
        return Err(format!("{} 不是 Git 仓库。", repo_path.display()));
    }
    let resolved = run_git(repo_path, &["rev-parse", "--verify", &format!("{}^{{commit}}", commit)])?;
    let resolved = resolved.trim().to_string();
    if resolved.len() != 40 || !resolved.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("无法解析 commit：{}", commit));
    }

    let matcher = Matcher::new(include, exclude)?;
    let listing = run_git_bytes(repo_path, &["ls-tree", "-r", "-z", "--full-tree", &resolved])?;
    let mut blobs: Vec<(String, String)> = Vec::new();
    for record in listing.split(|byte| *byte == 0) {
        if record.is_empty() {
            continue;
        }
        let record = std::str::from_utf8(record).map_err(|_| "仓库路径包含非 UTF-8 内容，已拒绝。".to_string())?;
        let (meta, path) = record.split_once('\t').ok_or_else(|| "ls-tree 输出格式异常。".to_string())?;
        let mut parts = meta.split_whitespace();
        let mode = parts.next().unwrap_or_default();
        let kind = parts.next().unwrap_or_default();
        let hash = parts.next().unwrap_or_default();
        if kind != "blob" {
            continue;
        }
        match mode {
            "100644" | "100755" => {}
            "120000" => return Err(format!("快照拒绝符号链接：{}", path)),
            _ => continue,
        }
        if !matcher.allows(path) {
            continue;
        }
        blobs.push((hash.to_string(), path.to_string()));
    }
    if blobs.is_empty() {
        return Err(format!("来源 {} 在 commit {} 没有匹配文件。", label, &resolved[..12]));
    }

    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法启动 git：{}", error))?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| "git stdin 不可用。".to_string())?;
        for (hash, _) in &blobs {
            writeln!(stdin, "{}", hash).map_err(|error| error.to_string())?;
        }
    }
    child.stdin.take();
    let mut stdout = child.stdout.take().ok_or_else(|| "git stdout 不可用。".to_string())?;

    let mut files = Vec::new();
    for (_, path) in &blobs {
        let header = read_line(&mut stdout)?;
        let mut parts = header.split_whitespace();
        let _hash = parts.next().ok_or_else(|| "cat-file 输出异常。".to_string())?;
        let kind = parts.next().ok_or_else(|| "cat-file 输出异常。".to_string())?;
        if kind == "missing" {
            return Err(format!("commit 缺少对象：{}", path));
        }
        let size: usize = parts
            .next()
            .ok_or_else(|| "cat-file 输出异常。".to_string())?
            .parse()
            .map_err(|_| "cat-file 输出异常。".to_string())?;
        let mut bytes = vec![0u8; size];
        stdout.read_exact(&mut bytes).map_err(|error| error.to_string())?;
        let mut trailing = [0u8; 1];
        stdout.read_exact(&mut trailing).map_err(|error| error.to_string())?;
        if let Some(reason) = scan_sensitive(&bytes) {
            return Err(format!("{} 命中敏感内容（{}），已拒绝构建。", path, reason));
        }
        files.push(CollectedFile {
            path: path.clone(),
            bytes,
            source_type: "git",
            source_path: path.clone(),
            repository: Some(label.to_string()),
            commit: Some(resolved.clone()),
        });
    }
    let _ = child.wait();

    Ok((
        files,
        SourceRecord {
            label: label.to_string(),
            source_type: "git",
            reference: resolved,
        },
    ))
}

fn collect_folder(label: &str, path: &Path, include: &[String]) -> Result<(Vec<CollectedFile>, SourceRecord), String> {
    let root = path
        .canonicalize()
        .map_err(|_| format!("目录不存在：{}", path.display()))?;
    if !root.is_dir() {
        return Err(format!("{} 不是目录。", root.display()));
    }
    let matcher = Matcher::new(include, &[])?;
    let mut files = Vec::new();
    walk_folder(&root, &root, &matcher, &mut files)?;
    if files.is_empty() {
        return Err(format!("目录 {} 没有匹配文件。", root.display()));
    }
    Ok((
        files,
        SourceRecord {
            label: label.to_string(),
            source_type: "folder",
            reference: root.display().to_string(),
        },
    ))
}

fn walk_folder(root: &Path, dir: &Path, matcher: &Matcher, files: &mut Vec<CollectedFile>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|error| error.to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    for entry in entries {
        let metadata = fs::symlink_metadata(&entry).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("快照拒绝符号链接：{}", entry.display()));
        }
        if metadata.is_dir() {
            walk_folder(root, &entry, matcher, files)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let relative = entry
            .strip_prefix(root)
            .map_err(|_| "目录遍历越界。".to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if !matcher.allows(&relative) {
            continue;
        }
        let bytes = fs::read(&entry).map_err(|error| error.to_string())?;
        if let Some(reason) = scan_sensitive(&bytes) {
            return Err(format!("{} 命中敏感内容（{}），已拒绝构建。", relative, reason));
        }
        files.push(CollectedFile {
            path: relative.clone(),
            bytes,
            source_type: "folder",
            source_path: relative,
            repository: None,
            commit: None,
        });
    }
    Ok(())
}

struct Matcher {
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}

impl Matcher {
    fn new(include: &[String], exclude: &[String]) -> Result<Self, String> {
        Ok(Self {
            include: build_globset(include)?,
            exclude: build_globset(exclude)?,
        })
    }

    fn allows(&self, path: &str) -> bool {
        if is_denied(path) {
            return false;
        }
        if let Some(exclude) = &self.exclude {
            if exclude.is_match(path) {
                return false;
            }
        }
        match &self.include {
            Some(include) => include.is_match(path),
            None => true,
        }
    }
}

fn build_globset(patterns: &[String]) -> Result<Option<GlobSet>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|_| format!("无效的匹配模式：{}", pattern))?;
        builder.add(glob);
    }
    builder.build().map(Some).map_err(|error| error.to_string())
}

fn is_denied(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if DEFAULT_DENY_PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return true;
    }
    if DEFAULT_DENY_PREFIXES.iter().any(|prefix| lower.contains(&format!("/{}", prefix))) {
        return true;
    }
    let name = lower.rsplit('/').next().unwrap_or("");
    if name.starts_with(".env") && name != ".env.example" {
        return true;
    }
    if DEFAULT_DENY_NAMES.contains(&name) {
        return true;
    }
    DEFAULT_DENY_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

fn is_rich_file(path: &str) -> bool {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some("pdf" | "docx" | "xlsx" | "pptx" | "png" | "jpg" | "jpeg" | "webp" | "tif" | "tiff" | "bmp" | "gif")
    )
}

fn scan_sensitive(content: &[u8]) -> Option<&'static str> {
    if contains(content, b"-----BEGIN ") && contains(content, b"PRIVATE KEY-----") {
        return Some("私钥块");
    }
    if contains(content, b"nsec1") {
        return Some("Nostr 私钥");
    }
    if contains_prefixed_run(content, b"sk-", 20) {
        return Some("疑似 API Key");
    }
    if contains_prefixed_run(content, b"AKIA", 16) {
        return Some("疑似 AWS 凭据");
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn contains_prefixed_run(haystack: &[u8], prefix: &[u8], min_run: usize) -> bool {
    let mut index = 0;
    while index + prefix.len() <= haystack.len() {
        if &haystack[index..index + prefix.len()] == prefix {
            let run = haystack[index + prefix.len()..]
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric())
                .count();
            if run >= min_run {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git_bytes(repo, args)?;
    String::from_utf8(output).map_err(|_| "git 输出不是 UTF-8。".to_string())
}

fn run_git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| format!("无法执行 git，请确认已安装 git：{}", error))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git 执行失败：{}", message));
    }
    Ok(output.stdout)
}

fn read_line(reader: &mut impl Read) -> Result<String, String> {
    let mut buffer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        reader.read_exact(&mut byte).map_err(|error| error.to_string())?;
        if byte[0] == b'\n' {
            break;
        }
        buffer.push(byte[0]);
    }
    String::from_utf8(buffer).map_err(|_| "git 输出不是 UTF-8。".to_string())
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    text.push('\n');
    fs::write(path, text).map_err(|error| error.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn lock_read_only(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    lock_read_only(&path);
                } else {
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o444));
                }
            }
        }
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o555));
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

fn utc_timestamp() -> String {
    let (year, month, day, hour, minute, second) = utc_parts();
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", year, month, day, hour, minute, second)
}

fn utc_iso() -> String {
    let (year, month, day, hour, minute, second) = utc_parts();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

fn utc_parts() -> (i64, u32, u32, u32, u32, u32) {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (time / 3600) as u32,
        ((time % 3600) / 60) as u32,
        (time % 60) as u32,
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir(name: &str) -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "fork-snapshot-test-{}-{}-{}",
            name,
            std::process::id(),
            unique
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn builds_folder_snapshot_and_skips_denied_paths() {
        let root = temp_dir("folder");
        let docs = root.join("docs");
        fs::create_dir_all(docs.join("sub")).unwrap();
        fs::write(docs.join("a.md"), "# A\nhello\n").unwrap();
        fs::write(docs.join("sub/b.md"), "# B\nworld\n").unwrap();
        fs::write(docs.join(".env"), "TOKEN=secret\n").unwrap();
        fs::create_dir_all(docs.join("node_modules")).unwrap();
        fs::write(docs.join("node_modules/x.js"), "x\n").unwrap();
        fs::write(docs.join("key.pem"), "-----BEGIN PRIVATE KEY-----\n").unwrap();

        let out = root.join("out");
        let source = KnowledgeSource::Folder {
            label: "docs".into(),
            path: docs.clone(),
            include: vec!["**/*.md".into()],
        };
        let outcome = build(&out, &[source]).unwrap();
        assert_eq!(outcome.file_count, 2);
        assert!(outcome.snapshot_dir.join("a.md").exists());
        assert!(outcome.snapshot_dir.join("sub/b.md").exists());
        assert!(!outcome.snapshot_dir.join(".env").exists());

        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&outcome.index_path).unwrap()).unwrap();
        assert_eq!(index["snapshot_id"], outcome.snapshot_id.as_str());
        assert_eq!(index["files"].as_array().unwrap().len(), 2);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_sensitive_content() {
        let root = temp_dir("sensitive");
        let docs = root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("leak.md"), "token: sk-abcdefghijklmnopqrstuvwx\n").unwrap();
        let out = root.join("out");
        let source = KnowledgeSource::Folder {
            label: "docs".into(),
            path: docs,
            include: vec![],
        };
        let error = build(&out, &[source]).unwrap_err();
        assert!(error.contains("敏感内容"), "unexpected error: {}", error);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_rich_file_too_large_for_runtime() {
        let root = temp_dir("large-rich-file");
        let docs = root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("large.PDF"), vec![0; MAX_RICH_FILE_BYTES + 1]).unwrap();
        let error = build(
            &root.join("out"),
            &[KnowledgeSource::Folder { label: "docs".into(), path: docs, include: vec![] }],
        )
        .unwrap_err();
        assert!(error.contains("16MB"), "unexpected error: {}", error);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_git_snapshot_from_commit_tree() {
        let root = temp_dir("git");
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README.md"), "# Repo\nversion 1\n").unwrap();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn one() {}\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        let first: String = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let first = first.trim().to_string();

        fs::write(repo.join("README.md"), "# Repo\nversion 2 uncommitted\n").unwrap();
        fs::write(repo.join("src/extra.rs"), "uncommitted\n").unwrap();

        let out = root.join("out");
        let source = KnowledgeSource::Git {
            label: "repo".into(),
            repo_path: repo.clone(),
            commit: first.clone(),
            include: vec![],
            exclude: vec![],
        };
        let outcome = build(&out, &[source]).unwrap();
        assert_eq!(outcome.file_count, 2);
        let readme = fs::read_to_string(outcome.snapshot_dir.join("README.md")).unwrap();
        assert!(readme.contains("version 1"), "working tree changes must not leak");
        assert!(!outcome.snapshot_dir.join("src/extra.rs").exists());

        let index: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&outcome.index_path).unwrap()).unwrap();
        let entry = index["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == "README.md")
            .unwrap();
        assert_eq!(entry["source_type"], "git");
        assert_eq!(entry["repository"], "repo");
        assert_eq!(entry["commit"], first.as_str());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks() {
        let root = temp_dir("symlink");
        let docs = root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("real.md"), "real\n").unwrap();
        std::os::unix::fs::symlink(docs.join("real.md"), docs.join("link.md")).unwrap();
        let out = root.join("out");
        let source = KnowledgeSource::Folder {
            label: "docs".into(),
            path: docs,
            include: vec![],
        };
        let error = build(&out, &[source]).unwrap_err();
        assert!(error.contains("符号链接"), "unexpected error: {}", error);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn repeated_build_in_same_second_is_rejected() {
        let root = temp_dir("unique");
        let docs = root.join("docs");
        fs::create_dir_all(&docs).unwrap();
        fs::write(docs.join("a.md"), "# A\n").unwrap();
        let out = root.join("out");
        let make = || KnowledgeSource::Folder {
            label: "docs".into(),
            path: docs.clone(),
            include: vec![],
        };
        let first = build(&out, &[make()]).unwrap();
        match build(&out, &[make()]) {
            Ok(second) => assert_ne!(second.snapshot_id, first.snapshot_id),
            Err(error) => assert!(error.contains("已存在"), "unexpected error: {}", error),
        }
        let _ = fs::remove_dir_all(&root);
    }
}
