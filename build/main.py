#!/usr/bin/env python3
"""ICBM 开发环境就绪脚本入口。

功能实现位于 `icbm_dev_env/` 包（build/ 下的分层模块），本文件只负责：
1. uv 自举：由 uv 管理 Python 环境（缺失时自动安装并以 uv 重新执行）；
2. CLI 解析：把子命令转发给 `icbm_dev_env.commands`。

用法：
    uv run build/main.py init
    python3 build/main.py init      # 无 uv 时亦可直接运行（当前仅依赖标准库）

代理：所有联网操作遵循 HTTPS_PROXY/HTTP_PROXY 环境变量（见 icbm_dev_env/net.py）。
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

from icbm_dev_env import config, toolchains


def _reexec_under_uv() -> None:
    """若已通过 uv 运行则跳过；否则安装/找到 uv 并重新执行本脚本。"""
    if os.environ.get("_ICBM_DEV_ENV_UV") == "1":
        return
    uv = toolchains.ensure_uv()
    if uv is None:
        return  # uv 不可用，回退为系统 python3 直接运行（仅依赖标准库）
    try:
        os.environ["_ICBM_DEV_ENV_UV"] = "1"
        cmd = [str(uv), "run", "--project", str(config.BUILD_DIR),
               str(Path(__file__))] + sys.argv[1:]
        os.execvpe(str(uv), cmd, os.environ)
    except OSError:
        print("[init] 警告: 以 uv 重新执行失败，回退为直接运行（当前仅依赖标准库）",
              file=sys.stderr, flush=True)


def main() -> None:
    _reexec_under_uv()
    from icbm_dev_env import commands, net  # 局部导入，避免 uv 自举阶段引入

    args = commands.build_parser().parse_args()
    try:
        args.func(args)
    except net.DownloadError as exc:
        print(f"[init] 错误: 下载失败: {exc}", file=sys.stderr, flush=True)
        sys.exit(1)


if __name__ == "__main__":
    main()
