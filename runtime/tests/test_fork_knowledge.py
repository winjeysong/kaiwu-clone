import hashlib
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from runtime.plugins import fork_knowledge


class KnowledgeSearchTest(unittest.TestCase):
    def test_skips_unreadable_file_without_weakening_read_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = {".gitignore": b".DS_Store\n", "answer.md": "知识库可搜索\n".encode()}
            entries = []
            for name, data in files.items():
                (root / name).write_bytes(data)
                entries.append({
                    "path": name,
                    "sha256": hashlib.sha256(data).hexdigest(),
                    "size_bytes": len(data),
                    "source_type": "folder",
                    "source_path": name,
                })
            index = root / "public-index.json"
            index.write_text(json.dumps({"schema_version": 1, "snapshot_id": "test", "files": entries}))

            with patch.object(fork_knowledge, "KNOWLEDGE_ROOT", root), patch.object(fork_knowledge, "INDEX_PATH", index):
                result = json.loads(fork_knowledge.knowledge_search("可搜索"))
                self.assertEqual(result["matches"][0]["path"], "answer.md")
                self.assertIn("file type is not approved", fork_knowledge.knowledge_read(".gitignore"))
                (root / "answer.md").write_text("内容已篡改")
                self.assertIn("does not match", fork_knowledge.knowledge_search("可搜索"))


if __name__ == "__main__":
    unittest.main()
