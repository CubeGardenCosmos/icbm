"""Loom 反编译：运行 Gradle genSources、解包源码、下载原版 jar。"""

from __future__ import annotations

import os
import zipfile
from pathlib import Path

from . import config, net, util


def decompile_minecraft(jdk_home: Path) -> Path:
    """运行 Loom genSources，返回反编译源码目录（minecraft-assets）。"""
    if not (config.LOOM_DIR / "settings.gradle").is_file():
        util.err(f"缺少 Loom 工程文件 {config.LOOM_DIR}，请确认已从仓库检出")
    wrapper = config.LOOM_DIR / ("gradlew.bat" if os.name == "nt" else "gradlew")
    if not wrapper.is_file():
        util.err(f"缺少 Gradle wrapper: {wrapper}")

    env = dict(os.environ)
    env["JAVA_HOME"] = str(jdk_home)
    env["PATH"] = str(jdk_home / "bin") + os.pathsep + env.get("PATH", "")
    cmd = [str(wrapper), "--console=plain"] + net.gradle_proxy_args() + ["exportSources"]
    util.run(cmd, cwd=config.LOOM_DIR, env=env)

    sources_jar = _locate_sources_jar()
    util.log(f"反编译产物: {sources_jar}")

    # 解包源码到 minecraft-assets（幂等：覆盖旧的 net/ 等目录）
    config.ASSETS_DIR.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(sources_jar) as zf:
        zf.extractall(config.ASSETS_DIR)
    java_count = sum(1 for _ in config.ASSETS_DIR.rglob("*.java"))
    util.log(f"已解包反编译源码到 {config.ASSETS_DIR}（Java 文件数: {java_count}）")
    return config.ASSETS_DIR


def fetch_vanilla_jars() -> None:
    """下载原版 client.jar / server.jar 到 minecraft-assets/jars/。"""
    config.ASSETS_JARS_DIR.mkdir(parents=True, exist_ok=True)
    client_url, server_url = net.mojang_jar_urls()
    client_jar = config.ASSETS_JARS_DIR / f"minecraft-client-{config.MC_VERSION}.jar"
    server_jar = config.ASSETS_JARS_DIR / f"minecraft-server-{config.MC_VERSION}.jar"
    if not client_jar.is_file():
        net.download(client_url, client_jar, "原版 client.jar")
    if not server_jar.is_file():
        net.download(server_url, server_jar, "原版 server.jar")


def _locate_sources_jar() -> Path:
    """按优先级定位 exportSources 产物。"""
    if config.EXPORTED_SOURCES_JAR.is_file():
        return config.EXPORTED_SOURCES_JAR
    for pattern in ("build/libs/*-sources.jar", "build/**/*-sources.jar"):
        jars = sorted(config.LOOM_DIR.glob(pattern))
        if jars:
            return jars[-1]
    util.err("未找到 genSources 产物（build/libs/*-sources.jar），请检查 Loom 日志")
