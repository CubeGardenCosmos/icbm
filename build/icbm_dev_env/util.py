"""基础工具：日志输出、命令执行与平台探测。"""

from __future__ import annotations

import platform
import subprocess
import sys
from pathlib import Path


def log(msg: str) -> None:
    print(f"[init] {msg}", flush=True)


def err(msg: str) -> None:
    print(f"[init] 错误: {msg}", file=sys.stderr, flush=True)
    sys.exit(1)


def run(cmd: list[str], *, cwd: Path | None = None, env: dict | None = None,
        check: bool = True) -> subprocess.CompletedProcess:
    """执行命令；失败时给出带上下文的错误。"""
    display = " ".join(str(c) for c in cmd)
    log(f"执行: {display}")
    try:
        proc = subprocess.run(cmd, cwd=cwd, env=env, text=True)
    except FileNotFoundError:
        err(f"找不到命令 {cmd[0]}，请检查 PATH 或先运行对应子命令")
    if check and proc.returncode != 0:
        err(f"命令失败（exit={proc.returncode}）: {display}")
    return proc


def os_arch_pair() -> tuple[str, str]:
    """归一化平台与架构（如 linux/x64、mac/aarch64），供工具链下载用。"""
    system = platform.system().lower()
    machine = platform.machine().lower()
    os_map = {"linux": "linux", "darwin": "mac", "windows": "win32"}
    arch_map = {"x86_64": "x64", "amd64": "x64", "aarch64": "aarch64", "arm64": "aarch64"}
    if system not in os_map or machine not in arch_map:
        err(f"不支持的平台: {system}/{machine}")
    return os_map[system], arch_map[machine]


class Steps:
    """init 分步进度显示（cargo 风格 `==> [i/n] 步骤 …` / `[i/n] ✓`）。"""

    def __init__(self, total: int) -> None:
        self.total = total
        self._i = 0

    def next(self, name: str) -> None:
        self._i += 1
        print(f"==> [{self._i}/{self.total}] {name} …", flush=True)

    def ok(self, detail: str = "") -> None:
        suffix = f"（{detail}）" if detail else ""
        print(f"    [{self._i}/{self.total}] 完成{suffix}", flush=True)
