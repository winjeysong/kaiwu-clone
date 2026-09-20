"""Hermes plugin exposing two allowlisted, read-only knowledge tools."""

import hashlib
import json
import os
import stat
from pathlib import Path, PurePosixPath


KNOWLEDGE_ROOT = Path("/knowledge")
INDEX_PATH = Path(os.environ.get("FORK_KNOWLEDGE_INDEX", "/run/secrets/fork-public-index"))
MAX_FILE_BYTES = 1_000_000
MAX_INDEX_BYTES = 2_000_000
MAX_LINES = 200
MAX_RESULTS = 50
TEXT_SUFFIXES = {
    ".cfg",
    ".conf",
    ".css",
    ".csv",
    ".html",
    ".ini",
    ".java",
    ".js",
    ".json",
    ".jsx",
    ".md",
    ".mdx",
    ".py",
    ".rst",
    ".sh",
    ".sql",
    ".toml",
    ".ts",
    ".tsx",
    ".txt",
    ".vue",
    ".xml",
    ".yaml",
    ".yml",
}
INTERNAL_FILES = {
    "MANIFEST.json",
    "document-index.runtime.md",
    "document-index.md",
    "evals.md",
    "public-index.json",
    "sources.json",
    "sources.yaml",
}
INTERNAL_PREFIXES = {"fork-config", "snapshot-receipts", "snapshots", "tests", "tools"}


def _result(**values):
    return json.dumps(values, ensure_ascii=False)


def _relative_path(value):
    if not isinstance(value, str) or not value or len(value) > 500:
        raise ValueError("path must be a non-empty relative path")
    if "\x00" in value or "\\" in value:
        raise ValueError("path contains forbidden characters")
    relative = PurePosixPath(value)
    if relative.is_absolute() or any(part in {"", ".", ".."} for part in relative.parts):
        raise ValueError("path must stay inside /knowledge")
    if value in INTERNAL_FILES or relative.parts[0] in INTERNAL_PREFIXES:
        raise ValueError("path is not published knowledge")
    return relative


def _load_index():
    if INDEX_PATH.is_symlink():
        raise ValueError("knowledge index must not be a symbolic link")
    try:
        mode = INDEX_PATH.lstat().st_mode
    except FileNotFoundError as error:
        raise ValueError("protected knowledge index is unavailable") from error
    if not stat.S_ISREG(mode) or INDEX_PATH.stat().st_size > MAX_INDEX_BYTES:
        raise ValueError("protected knowledge index is invalid")
    try:
        payload = json.loads(INDEX_PATH.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, ValueError, TypeError) as error:
        raise ValueError("protected knowledge index is invalid") from error
    if payload.get("schema_version") != 1 or not isinstance(payload.get("snapshot_id"), str):
        raise ValueError("protected knowledge index is invalid")
    entries = {}
    for entry in payload.get("files", []):
        if not isinstance(entry, dict):
            raise ValueError("protected knowledge index is invalid")
        path = entry.get("path")
        if not isinstance(path, str):
            raise ValueError("protected knowledge index is invalid")
        _relative_path(path)
        digest = entry.get("sha256")
        size = entry.get("size_bytes")
        source_type = entry.get("source_type")
        if (
            path in entries
            or source_type not in {"generated", "git", "folder"}
            or not isinstance(digest, str)
            or len(digest) != 64
            or any(character not in "0123456789abcdef" for character in digest)
            or not isinstance(size, int)
            or isinstance(size, bool)
            or not 0 <= size <= MAX_FILE_BYTES
        ):
            raise ValueError("protected knowledge index is invalid")
        if source_type == "git":
            repository = entry.get("repository")
            commit = entry.get("commit")
            source_path = entry.get("source_path")
            if (
                not isinstance(repository, str)
                or not repository
                or not isinstance(source_path, str)
                or not source_path
                or not isinstance(commit, str)
                or len(commit) != 40
                or any(character not in "0123456789abcdef" for character in commit)
            ):
                raise ValueError("protected knowledge index is invalid")
        elif entry.get("source_path") != path:
            raise ValueError("protected knowledge index is invalid")
        entries[path] = entry
    if not entries:
        raise ValueError("protected knowledge index is empty")
    return entries


def _approved_file(path, entries):
    relative = _relative_path(path)
    key = relative.as_posix()
    entry = entries.get(key)
    if entry is None:
        raise ValueError("path is not in the protected knowledge allowlist")

    if KNOWLEDGE_ROOT.is_symlink():
        raise ValueError("knowledge root must not be a symbolic link")
    root = KNOWLEDGE_ROOT.resolve(strict=True)
    source = KNOWLEDGE_ROOT
    for part in relative.parts:
        source /= part
        if source.is_symlink():
            raise ValueError("symbolic links are forbidden")
    resolved = source.resolve(strict=True)
    resolved.relative_to(root)
    if not resolved.is_file() or resolved.suffix.lower() not in TEXT_SUFFIXES:
        raise ValueError("file type is not approved for text reading")
    data = resolved.read_bytes()
    if len(data) != entry["size_bytes"] or hashlib.sha256(data).hexdigest() != entry["sha256"]:
        raise ValueError("published file does not match the protected knowledge index")
    if b"\x00" in data:
        raise ValueError("binary content is forbidden")
    return root, resolved, entry, data.decode("utf-8")


def _citation(entry, path, start_line, end_line, lines):
    if entry.get("source_type") == "git":
        location = str(start_line) if start_line == end_line else f"{start_line}-{end_line}"
        return f"{entry['repository']}@{entry['commit']}:{entry['source_path']}:{location}"

    heading = next(
        (
            line.lstrip()[1:].lstrip("# ").strip()
            for line in reversed(lines[:start_line])
            if line.lstrip().startswith("#")
        ),
        f"L{start_line}-L{end_line}",
    )
    return f"knowledge@sha256:{path}#{heading}"


def knowledge_read(path, start_line=1, line_count=MAX_LINES):
    try:
        if not isinstance(start_line, int) or isinstance(start_line, bool) or start_line < 1:
            raise ValueError("start_line must be a positive integer")
        if not isinstance(line_count, int) or isinstance(line_count, bool) or not 1 <= line_count <= MAX_LINES:
            raise ValueError(f"line_count must be between 1 and {MAX_LINES}")
        entries = _load_index()
        root, source, entry, text = _approved_file(path, entries)
        lines = text.splitlines()
        selected = lines[start_line - 1 : start_line - 1 + line_count]
        end_line = start_line + len(selected) - 1 if selected else start_line - 1
        relative = source.relative_to(root).as_posix()
        return _result(
            path=relative,
            start_line=start_line,
            end_line=end_line,
            citation=_citation(entry, relative, start_line, end_line, lines),
            content="\n".join(f"{number}: {line}" for number, line in enumerate(selected, start_line)),
        )
    except (OSError, UnicodeError, ValueError) as error:
        return _result(error=str(error))


def knowledge_search(query, max_results=20):
    try:
        if not isinstance(query, str) or not query.strip() or len(query) > 200:
            raise ValueError("query must contain 1 to 200 characters")
        if not isinstance(max_results, int) or isinstance(max_results, bool) or not 1 <= max_results <= MAX_RESULTS:
            raise ValueError(f"max_results must be between 1 and {MAX_RESULTS}")
        entries = _load_index()
        needle = query.casefold()
        matches = []
        for relative, entry in sorted(entries.items()):
            if Path(relative).suffix.lower() not in TEXT_SUFFIXES:
                continue
            root, source, _, text = _approved_file(relative, entries)
            lines = text.splitlines()
            for line_number, line in enumerate(lines, 1):
                if needle in line.casefold():
                    path = source.relative_to(root).as_posix()
                    matches.append(
                        {
                            "path": path,
                            "line": line_number,
                            "citation": _citation(entry, path, line_number, line_number, lines),
                            "text": line.strip()[:400],
                        }
                    )
                    if len(matches) == max_results:
                        return _result(query=query, matches=matches, truncated=True)
        return _result(query=query, matches=matches, truncated=False)
    except (OSError, UnicodeError, ValueError) as error:
        return _result(error=str(error))


READ_SCHEMA = {
    "name": "knowledge_read",
    "description": "Read numbered lines from one file in the protected knowledge allowlist. The result includes a verified citation. Unlisted, changed, absolute, escaping and symbolic-link paths are rejected.",
    "parameters": {
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Published knowledge path from a search result"},
            "start_line": {"type": "integer", "minimum": 1, "default": 1},
            "line_count": {"type": "integer", "minimum": 1, "maximum": MAX_LINES, "default": MAX_LINES},
        },
        "required": ["path"],
        "additionalProperties": False,
    },
}

SEARCH_SCHEMA = {
    "name": "knowledge_search",
    "description": "Literal case-insensitive search across the protected knowledge allowlist. Every match includes a verified citation. It cannot enumerate directories or search unlisted files.",
    "parameters": {
        "type": "object",
        "properties": {
            "query": {"type": "string", "minLength": 1, "maxLength": 200},
            "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "default": 20},
        },
        "required": ["query"],
        "additionalProperties": False,
    },
}


def register(ctx):
    ctx.register_tool(
        name="knowledge_read",
        toolset="fork_knowledge_readonly",
        schema=READ_SCHEMA,
        handler=lambda args, **_: knowledge_read(
            args.get("path"), args.get("start_line", 1), args.get("line_count", MAX_LINES)
        ),
    )
    ctx.register_tool(
        name="knowledge_search",
        toolset="fork_knowledge_readonly",
        schema=SEARCH_SCHEMA,
        handler=lambda args, **_: knowledge_search(
            args.get("query"), args.get("max_results", 20)
        ),
    )
