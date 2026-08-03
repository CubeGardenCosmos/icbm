"""子命令编排层：把 CLI 子命令映射为对领域模块（decompile/vcs/toolchains）的调用。

本层不做具体实现，只负责流程编排与用户交互（询问/确认）。
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

from . import config, decompile, net, toolchains, util, vcs


# --- init ---------------------------------------------------------------------

def cmd_init(args: argparse.Namespace) -> None:
    util.log(f"初始化 ICBM 开发环境（MC {config.MC_VERSION} / 协议 767）")
    config.WORKSPACE.mkdir(parents=True, exist_ok=True)
    steps = util.Steps(8)

    steps.next("同步 Git 子模块")
    vcs.sync_submodules()
    steps.ok()

    steps.next("克隆参考仓库（Pumpkin / NeoForge）")
    vcs.clone_repos()
    steps.ok()

    steps.next("部署 JDK 21")
    jdk_home = toolchains.provision_jdk(force_workspace=args.workspace_jdk)
    steps.ok(str(jdk_home))

    steps.next("Loom 反编译并解包源码")
    decompile.decompile_minecraft(jdk_home)
    steps.ok()

    steps.next("下载原版 client / server jar")
    decompile.fetch_vanilla_jars()
    steps.ok()

    steps.next("迁移家目录旧反编译资产")
    vcs.migrate_legacy_assets()
    steps.ok()

    if args.with_tools:
        steps.next("安装基础软件（Node / Rust 工具链 / pi）")
        toolchains.install_basic_software()
        steps.ok()
    elif sys.stdin.isatty():
        steps.next("基础软件安装（可跳过）")
        answer = input("是否安装基础软件库（Rust 工具链 cargo/rustc/rustfmt、Node、pi）？[y/N] ")
        if answer.strip().lower() in ("y", "yes"):
            toolchains.install_basic_software()
            steps.ok()
        else:
            steps.ok("已跳过")
    else:
        util.log("非交互环境，跳过基础软件安装（可用 --with-tools 显式安装）")

    steps.next("预热 cargo release 编译缓存")
    toolchains.warm_release_cache()
    steps.ok()

    util.log("开发环境初始化完成。反编译源码位于 build/workspace/minecraft-assets，"
             "Pumpkin/NeoForge 参考位于 build/workspace/。均已被 Git 忽略。")


# --- 单步子命令 ----------------------------------------------------------------

def cmd_submodules(_: argparse.Namespace) -> None:
    vcs.sync_submodules()


def cmd_clones(_: argparse.Namespace) -> None:
    vcs.clone_repos()


def cmd_jdk(args: argparse.Namespace) -> None:
    jdk_home = toolchains.provision_jdk(force_workspace=args.workspace_jdk)
    util.log(f"JDK: {jdk_home}")


def cmd_decompile(args: argparse.Namespace) -> None:
    jdk_home = toolchains.provision_jdk(force_workspace=args.workspace_jdk)
    decompile.decompile_minecraft(jdk_home)
    decompile.fetch_vanilla_jars()


def cmd_migrate_legacy(_: argparse.Namespace) -> None:
    vcs.migrate_legacy_assets()


def cmd_tools(_: argparse.Namespace) -> None:
    toolchains.install_basic_software()


def cmd_rustfmt(_: argparse.Namespace) -> None:
    toolchains.ensure_rust()


def cmd_status(_: argparse.Namespace) -> None:
    def fmt_dir(p: Path) -> str:
        return str(p) if p.exists() else f"{p}（缺失）"

    print("=== ICBM 开发环境状态 ===")
    print(f"工作区: {fmt_dir(config.WORKSPACE)}")
    print(f"反编译源码: {fmt_dir(config.ASSETS_DIR)}")
    if config.ASSETS_DIR.exists():
        fresh = sum(1 for p in config.ASSETS_DIR.rglob("*.java") if "legacy" not in p.parts)
        print(f"  Java 文件数（不含 legacy 旧资产）: {fresh}")
    for name, dest in [("Pumpkin", config.PUMPKIN_DIR), ("NeoForge", config.NEOFORGE_DIR)]:
        head = ""
        if (dest / ".git").exists():
            try:
                out = subprocess.run(["git", "-C", str(dest), "rev-parse", "--short", "HEAD"],
                                     capture_output=True, text=True, timeout=15).stdout.strip()
                head = f" @ {out}"
            except Exception:  # noqa: BLE001
                pass
        print(f"{name}: {fmt_dir(dest)}{head}")
    jdk = toolchains.find_system_jdk()
    print(f"系统 JDK: {jdk or '未找到 21+（init 时会在 workspace 部署 Temurin 21）'}")
    if (config.JDK_DIR / "bin" / "java").is_file():
        print(f"workspace JDK: {config.JDK_DIR}")
    node = shutil.which("node")
    print(f"Node: {node or f'未安装（init 会部署到 {config.NODE_DIR}）'}")
    print(f"rustfmt: {shutil.which('rustfmt') or '未安装'}")
    print(f"rustup: {'已安装' if shutil.which('rustup') else '未安装'}")
    print(f"pi: {shutil.which('pi') or '未安装'}")
    print(f"uv: {shutil.which('uv') or '未安装（脚本会自动安装并改用 uv 运行）'}")
    proxy = net.detect_proxy()
    print(f"代理: {proxy or '未设置（直连）'}")


def cmd_clean(args: argparse.Namespace) -> None:
    targets = [config.WORKSPACE, config.LOOM_DIR / ".gradle",
               config.LOOM_DIR / "build", config.BUILD_DIR / ".venv"]
    if not args.yes:
        answer = input("确认删除 build/workspace、Loom 构建产物与 build/.venv？[y/N] ")
        if answer.strip().lower() not in ("y", "yes"):
            util.log("已取消")
            return
    for p in targets:
        if p.exists():
            shutil.rmtree(p)
            util.log(f"已删除 {p}")


# --- CLI 构建 -------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="build/main.py",
        description="ICBM 开发环境就绪脚本（反编译 MC 1.21.1、克隆参考仓库、部署工具链）。",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    p_init = sub.add_parser("init", help="完整初始化开发环境")
    p_init.add_argument("--workspace-jdk", action="store_true",
                        help="强制使用 workspace 内部署的 JDK，忽略系统 JDK")
    p_init.add_argument("--with-tools", action="store_true",
                        help="跳过询问，直接安装基础软件（Node/Rust/pi）")
    p_init.set_defaults(func=cmd_init)

    sub.add_parser("submodules", help="仅同步 Git 子模块").set_defaults(func=cmd_submodules)
    sub.add_parser("clones", help="仅克隆 Pumpkin / NeoForge 参考仓库").set_defaults(func=cmd_clones)

    p_jdk = sub.add_parser("jdk", help="确保 JDK 21 可用（缺失时部署到 workspace）")
    p_jdk.add_argument("--workspace-jdk", action="store_true")
    p_jdk.set_defaults(func=cmd_jdk)

    p_dec = sub.add_parser("decompile", help="仅执行 Loom 反编译并下载原版 jar")
    p_dec.add_argument("--workspace-jdk", action="store_true")
    p_dec.set_defaults(func=cmd_decompile)

    sub.add_parser("migrate-legacy",
                   help="将家目录旧反编译资产迁入 minecraft-assets/legacy").set_defaults(func=cmd_migrate_legacy)
    sub.add_parser("tools", help="安装基础软件（Node/Rust 工具链/pi）").set_defaults(func=cmd_tools)
    sub.add_parser("rustfmt", help="确保 rustfmt / clippy 组件可用").set_defaults(func=cmd_rustfmt)
    sub.add_parser("status", help="查看开发环境状态").set_defaults(func=cmd_status)

    p_clean = sub.add_parser("clean", help="删除 build/workspace 与 Loom 构建产物")
    p_clean.add_argument("-y", "--yes", action="store_true", help="跳过确认")
    p_clean.set_defaults(func=cmd_clean)

    return parser
