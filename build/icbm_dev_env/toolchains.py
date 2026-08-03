"""工具链部署：uv / JDK 21 / Node / Rust / pi。

优先复用系统已装工具；缺失时统一部署到 `build/workspace/.tools/`，
保证客户机环境可复现、不依赖本机特定配置。
"""

from __future__ import annotations

import glob
import os
import re
import shutil
import subprocess
from pathlib import Path

from . import archives, config, net, util

_JDK_MIN_MAJOR = 21


# --- uv（Python 依赖管理器） --------------------------------------------------

def ensure_uv() -> Path | None:
    """确保 uv 可用；缺失时安装。失败返回 None（回退为系统 python3 直接运行）。"""
    uv = shutil.which("uv")
    if uv:
        return Path(uv)
    util.log("未找到 uv（依赖管理器），将安装…")
    try:
        script = config.TOOLS_DIR / "uv-install.sh"
        net.download(config.UV_INSTALL_URL, script, "uv 安装脚本")
        util.run(["sh", str(script)], env=dict(os.environ))
    except SystemExit:
        util.log("警告: uv 安装失败，将直接用系统 python3 运行（当前仅依赖标准库）")
        return None
    return Path(shutil.which("uv") or (Path.home() / ".local" / "bin" / "uv"))


# --- JDK 21 -------------------------------------------------------------------

def java_major(java_bin: Path) -> int | None:
    try:
        out = subprocess.run([str(java_bin), "-version"], capture_output=True,
                             text=True, timeout=30).stderr
        m = re.search(r'version "(\d+)', out)
        return int(m.group(1)) if m else None
    except Exception:  # noqa: BLE001 - 探测失败视为不可用
        return None


def find_system_jdk() -> Path | None:
    """在 PATH / JAVA_HOME / 常见安装路径中找 Java 21+，返回其根目录。"""
    candidates: list[Path] = []
    java_home = os.environ.get("JAVA_HOME")
    if java_home:
        candidates.append(Path(java_home) / "bin" / "java")
    which = shutil.which("java")
    if which:
        candidates.append(Path(which))
    home = Path.home()
    for pattern in [
        "/usr/lib/jvm/*/bin/java",
        "/usr/lib/jvm/java-*/bin/java",
        "/Library/Java/JavaVirtualMachines/*/Contents/Home/bin/java",
        str(home / ".jdks" / "*" / "bin" / "java"),
        str(home / ".sdkman" / "candidates" / "java" / "*" / "bin" / "java"),
        str(home / "jdk-*" / "bin" / "java"),
    ]:
        candidates.extend(Path(p).resolve() for p in glob.glob(pattern))
    seen: set[str] = set()
    for cand in candidates:
        key = str(cand)
        if key in seen:
            continue
        seen.add(key)
        major = java_major(cand)
        if major is not None and major >= _JDK_MIN_MAJOR:
            util.log(f"找到系统 JDK {major}：{cand}")
            return cand.parent.parent
    return None


def provision_jdk(force_workspace: bool = False) -> Path:
    """返回可用 JDK 21+ 根目录；必要时下载 Temurin 21 到 workspace/.tools。

    官方 Adoptium 源（经 github.com 分发）失败时自动降级清华 TUNA 镜像。
    """
    if not force_workspace:
        found = find_system_jdk()
        if found:
            return found
        util.log("未找到系统 JDK 21+，将在 workspace 中部署 Temurin 21")
    deployed = config.JDK_DIR / "bin" / "java"
    if deployed.is_file() and (java_major(deployed) or 0) >= _JDK_MIN_MAJOR:
        return config.JDK_DIR
    os_name, arch = util.os_arch_pair()
    suffix = "zip" if os_name == "win32" else "tar.gz"
    archive = config.TOOLS_DIR / f"temurin-21-{os_name}-{arch}.{suffix}"
    primary = config.ADOPTIUM_API.format(os=os_name, arch=arch)
    try:
        net.download(primary, archive, "Temurin JDK 21")
    except net.DownloadError:
        util.log("Adoptium 官方源下载失败，降级清华 TUNA 镜像…")
        net.download(net.adoptium_tuna_url(os_name, arch), archive, "Temurin JDK 21（TUNA 镜像）")
    _deploy_from_archive(archive, config.JDK_DIR)
    if (java_major(config.JDK_DIR / "bin" / "java") or 0) < _JDK_MIN_MAJOR:
        util.err("部署的 JDK 版本校验失败")
    util.log(f"JDK 21 已部署到 {config.JDK_DIR}")
    return config.JDK_DIR


# --- Node LTS ------------------------------------------------------------------

def ensure_node() -> Path | None:
    """返回可用 Node 根目录；缺失时部署 Node LTS 到 workspace/.tools/node。"""
    which = shutil.which("node")
    if which:
        util.log(f"检测到系统 Node: {which}")
        return Path(which).resolve().parent.parent
    if (config.NODE_DIR / "bin" / "node").is_file():
        return config.NODE_DIR
    util.log("未找到 Node，将在 workspace 中部署 Node LTS")
    version = net.node_latest_lts_version()
    os_name, arch = util.os_arch_pair()
    if os_name == "win32":
        file_name = f"node-{version}-win-{arch}.zip"
    elif os_name == "mac":
        file_name = f"node-{version}-darwin-{arch}.tar.gz"
    else:
        file_name = f"node-{version}-linux-{arch}.tar.xz"
    archive = config.TOOLS_DIR / file_name
    net.download(f"https://nodejs.org/dist/{version}/{file_name}", archive, f"Node {version}")
    _deploy_from_archive(archive, config.NODE_DIR)
    util.log(f"Node 已部署到 {config.NODE_DIR}")
    return config.NODE_DIR


# --- Rust 工具链 ----------------------------------------------------------------

def ensure_rust() -> None:
    """确保 Rust 工具链（cargo/rustc/rustfmt/clippy）可用。"""
    if shutil.which("rustup"):
        util.log("确保 rustfmt / clippy 组件（rustup component add）…")
        util.run(["rustup", "component", "add", "rustfmt", "clippy"], env=dict(os.environ))
        return
    if shutil.which("cargo"):
        util.log("检测到 cargo（未用 rustup 管理），rustfmt/clippy 请按发行版方式安装")
        return
    util.log("未找到 rustup，将安装 minimal 工具链（cargo/rustc/rustfmt/clippy）…")
    script = config.TOOLS_DIR / "rustup-init.sh"
    net.download(config.RUSTUP_INIT_URL, script, "rustup 安装脚本")
    util.run(["sh", str(script), "-y", "--profile", "minimal",
              "--component", "rustfmt,clippy", "--no-modify-path"], env=dict(os.environ))
    if not shutil.which("rustup"):
        util.err("rustup 安装后仍不在 PATH，请重新打开终端或手动配置 ~/.cargo/bin")
    util.log("cargo / rustc / rustfmt / clippy 就绪")


# --- pi（AI 开发工具） -------------------------------------------------------------

def install_pi(node_home: Path | None) -> None:
    """安装 pi（https://pi.dev/install.sh）；Node 缺失时先部署 Node。"""
    if shutil.which("pi"):
        util.log(f"pi 已安装: {shutil.which('pi')}")
        return
    util.log("安装 pi（AI 开发工具）…")
    env = dict(os.environ)
    if node_home is not None and (node_home / "bin" / "node").is_file():
        env["PATH"] = str(node_home / "bin") + os.pathsep + env.get("PATH", "")
    script = config.TOOLS_DIR / "pi-install.sh"
    net.download(config.PI_INSTALL_URL, script, "pi 安装脚本")
    util.run(["sh", str(script)], env=env)
    if shutil.which("pi"):
        util.log("pi 安装完成")
    else:
        util.log("提示: 未在 PATH 中找到 pi，请重新打开终端后确认")


def install_basic_software() -> None:
    """基础软件库：Node（缺失时部署）+ Rust 工具链（cargo/rustc/rustfmt/clippy）+ pi。"""
    util.log("安装基础软件库：Node / Rust 工具链（cargo、rustc、rustfmt、clippy）/ pi")
    node_home = ensure_node()
    ensure_rust()
    install_pi(node_home)


# --- cargo 编译缓存预热 ----------------------------------------------------------

def warm_release_cache() -> None:
    """init 末尾执行一次 cargo build --release 预热编译缓存；成败均不阻断 init。"""
    if not shutil.which("cargo"):
        util.log("未找到 cargo，跳过 release 编译缓存预热")
        return
    util.log("预热 release 编译缓存（cargo build --release）…")
    proc = util.run(["cargo", "build", "--release"], cwd=config.REPO_ROOT, check=False)
    if proc.returncode == 0:
        util.log("cargo build --release 完成，编译缓存已就绪")
    else:
        util.log(f"cargo build --release 未成功（exit={proc.returncode}），缓存部分就绪")


# --- 内部工具 -------------------------------------------------------------------

def _deploy_from_archive(archive: Path, target: Path) -> None:
    """把下载的 JDK/Node 归档解压并改名为目标目录（幂等）。"""
    extract_dir = target.parent / f".{target.name}-extract"
    if extract_dir.exists():
        shutil.rmtree(extract_dir)
    archives.extract_archive(archive, extract_dir)
    extracted = next(p for p in extract_dir.iterdir() if p.is_dir())
    if target.exists():
        shutil.rmtree(target)
    extracted.rename(target)
    shutil.rmtree(extract_dir)
