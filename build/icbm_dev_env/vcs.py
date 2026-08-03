"""版本控制相关：子模块同步、参考仓库克隆（非子模块）、家目录旧资产迁移。"""

from __future__ import annotations

import os
import shutil
from pathlib import Path

from . import config, util


def sync_submodules() -> None:
    """同步 Git 子模块（如 lib/datarouter）。"""
    util.log("同步子模块（git submodule update --init --recursive）…")
    util.run(["git", "submodule", "update", "--init", "--recursive"],
             cwd=config.REPO_ROOT, env=dict(os.environ))


def clone_repos() -> None:
    """克隆外部参考仓库（普通克隆，非子模块；固定到与项目一致的分支）。"""
    clone_if_missing(config.PUMPKIN_URL, config.PUMPKIN_DIR, "Pumpkin 参考实现",
                     branch=config.PUMPKIN_BRANCH)
    clone_if_missing(config.NEOFORGE_URL, config.NEOFORGE_DIR, "NeoForge 参考实现",
                     branch=config.NEOFORGE_BRANCH)


def clone_if_missing(url: str, dest: Path, desc: str, *, branch: str | None = None) -> None:
    if (dest / ".git").exists():
        util.log(f"{desc} 已存在: {dest}")
        return
    if dest.exists():
        util.err(f"{dest} 已存在但不是 Git 仓库，请手动处理后重试")
    dest.parent.mkdir(parents=True, exist_ok=True)
    # git 原生读取 HTTP(S)_PROXY 环境变量，环境透传即可
    cmd = ["git", "clone", "--depth", "1"]
    if branch:
        cmd += ["--branch", branch]
    cmd += [url, str(dest)]
    util.run(cmd, env=dict(os.environ))


def migrate_legacy_assets() -> None:
    """把本机家目录中旧的 Loom/NeoForm 反编译资产迁入 minecraft-assets/legacy/。

    其它机器上没有这些目录时自动跳过；目标已存在时跳过（幂等）。
    """
    config.ASSETS_LEGACY_DIR.mkdir(parents=True, exist_ok=True)
    home = Path.home()
    for src_name, dest_name in config.LEGACY_HOME_ASSETS:
        src = home / src_name
        dest = config.ASSETS_LEGACY_DIR / dest_name
        if not src.is_dir():
            continue
        if dest.exists():
            util.log(f"旧资产已迁移过: {dest}")
            continue
        util.log(f"迁移 {src} -> {dest}")
        shutil.move(str(src), str(dest))
    remaining = [n for n, _ in config.LEGACY_HOME_ASSETS if (home / n).is_dir()]
    if remaining:
        util.log(f"提示: 家目录仍有反编译资产未迁移: {remaining}")
