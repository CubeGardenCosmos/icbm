"""归档解压：tar.gz / tar.xz / zip（纯标准库，避免依赖系统 tar/xz）。"""

from __future__ import annotations

import tarfile
import zipfile
from pathlib import Path


def extract_archive(archive: Path, dest: Path) -> None:
    """解压归档到 dest 目录（自动按扩展名选择格式）。"""
    dest.mkdir(parents=True, exist_ok=True)
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(archive) as zf:
            zf.extractall(dest)
        return
    mode = "r:xz" if archive.name.endswith(".xz") else "r:gz"
    with tarfile.open(archive, mode) as tf:
        tf.extractall(dest, filter="data")
