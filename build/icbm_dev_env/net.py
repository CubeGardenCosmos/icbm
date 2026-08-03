"""网络层：HTTP(S) 代理、下载（带进度条）与外部数据源（Mojang、Node、Adoptium TUNA 镜像）。

所有联网操作必须经本模块，统一遵循 HTTPS_PROXY/HTTP_PROXY 环境变量；
Gradle/JVM 不读环境代理变量，见 [`gradle_proxy_args`]。
"""

from __future__ import annotations

import json
import os
import re
import shutil
import sys
import time
import urllib.request
from pathlib import Path
from urllib.parse import urlsplit

from . import config, util


class DownloadError(RuntimeError):
    """下载失败（重试耗尽），供调用方降级（如切镜像）。"""


_ACTIVE_BAR: "_Progress | None" = None


def _bar_clear() -> None:
    """进度条占用行时先清行，避免与日志输出串行。"""
    global _ACTIVE_BAR
    if _ACTIVE_BAR is not None:
        sys.stdout.write("\r\x1b[K")
        sys.stdout.flush()
        _ACTIVE_BAR = None


class _Progress:
    """cargo 风格下载进度条（仅 TTY 显示，非交互环境静默）。"""

    _WIDTH = 30

    def __init__(self, desc: str, total: int | None) -> None:
        self.desc = desc
        self.total = total
        self.done = 0
        self._tty = sys.stdout.isatty()
        self._last = 0.0
        global _ACTIVE_BAR
        _ACTIVE_BAR = self if self._tty else None

    @staticmethod
    def _fmt(n: int) -> str:
        return f"{n / 1024 / 1024:.1f} MiB"

    def update(self, n: int) -> None:
        self.done += n
        if not self._tty:
            return
        now = time.monotonic()
        if now - self._last < 0.15:
            return
        self._last = now
        self.render()

    def render(self) -> None:
        if self.total:
            frac = self.done / self.total
            filled = int(frac * self._WIDTH)
            bar = "#" * filled + "-" * (self._WIDTH - filled)
            line = (f"\r[init] 下载 {self.desc}: {self._fmt(self.done)}/{self._fmt(self.total)}"
                    f" [{bar}] {frac * 100:5.1f}%")
        else:
            line = f"\r[init] 下载 {self.desc}: {self._fmt(self.done)}"
        sys.stdout.write(line)
        sys.stdout.flush()

    def finish(self) -> None:
        global _ACTIVE_BAR
        if self._tty:
            self.render()
            sys.stdout.write("\n")
            sys.stdout.flush()
        _ACTIVE_BAR = None


def detect_proxy() -> str | None:
    """从环境变量检测代理（优先 HTTPS，兼容大小写）。"""
    for key in ("HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"):
        val = os.environ.get(key)
        if val:
            return val
    return None


def build_opener() -> urllib.request.OpenerDirector:
    """urllib opener：检测到代理时显式注入（urllib 默认只认小写环境变量）。"""
    proxy = detect_proxy()
    if proxy:
        return urllib.request.build_opener(urllib.request.ProxyHandler(
            {"http": proxy, "https": proxy}))
    return urllib.request.build_opener()


def gradle_proxy_args() -> list[str]:
    """把代理环境变量翻译成 Gradle/JVM 的 -D 参数（JVM 网络不读环境代理变量）。"""
    proxy = detect_proxy()
    if not proxy:
        return []
    parsed = urlsplit(proxy if "://" in proxy else "http://" + proxy)
    if not parsed.hostname:
        return []
    host, port = parsed.hostname, parsed.port or (443 if parsed.scheme == "https" else 80)
    args = [f"-Dhttps.proxyHost={host}", f"-Dhttps.proxyPort={port}",
            f"-Dhttp.proxyHost={host}", f"-Dhttp.proxyPort={port}"]
    if parsed.username:
        password = parsed.password or ""
        args += [f"-Dhttps.proxyUser={parsed.username}", f"-Dhttps.proxyPassword={password}",
                 f"-Dhttp.proxyUser={parsed.username}", f"-Dhttp.proxyPassword={password}"]
    no_proxy = os.environ.get("NO_PROXY") or os.environ.get("no_proxy")
    if no_proxy:
        hosts = "|".join(p.strip() for p in no_proxy.split(",") if p.strip())
        if hosts:
            args.append(f"-Dhttp.nonProxyHosts={hosts}")
    return args


def download(url: str, dest: Path, desc: str) -> None:
    """下载文件到 dest，失败重试（重试耗尽抛 [`DownloadError`]），带进度条。"""
    dest.parent.mkdir(parents=True, exist_ok=True)
    opener = build_opener()
    req = urllib.request.Request(url, headers={"User-Agent": config.USER_AGENT})
    last_exc: Exception | None = None
    for attempt in range(1, config.RETRY_ATTEMPTS + 1):
        _bar_clear()
        util.log(f"下载 {desc}: {url}")
        try:
            with opener.open(req, timeout=180) as resp:
                total = resp.headers.get("Content-Length")
                progress = _Progress(desc, int(total) if total else None)
                with open(dest, "wb") as fh:
                    while True:
                        chunk = resp.read(256 * 1024)
                        if not chunk:
                            break
                        fh.write(chunk)
                        progress.update(len(chunk))
                progress.finish()
            util.log(f"完成下载 {desc} ({dest.stat().st_size / 1024 / 1024:.1f} MiB)")
            return
        except Exception as exc:  # noqa: BLE001 - 统一重试
            last_exc = exc
            _bar_clear()
            util.log(f"下载 {desc} 第 {attempt} 次失败（{exc}），重试…")
            time.sleep(2 * attempt)
    raise DownloadError(f"{desc}: {last_exc}")


def download_text(url: str) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": config.USER_AGENT})
    with build_opener().open(req, timeout=120) as resp:
        return resp.read().decode("utf-8")


# --- 外部数据源 ---------------------------------------------------------------

def mojang_jar_urls() -> tuple[str, str]:
    """Mojang 版本清单中 1.21.1 的 client.jar / server.jar 下载地址。"""
    manifest = json.loads(download_text(config.VERSION_MANIFEST_URL))
    entry = next((v for v in manifest["versions"] if v["id"] == config.MC_VERSION), None)
    if entry is None:
        util.err(f"版本清单中找不到 {config.MC_VERSION}")
    version_json = json.loads(download_text(entry["url"]))
    dl = version_json["downloads"]
    return dl["client"]["url"], dl["server"]["url"]


def node_latest_lts_version() -> str:
    """Node 官方索引中最新 LTS 版本号（形如 v24.x.y）。"""
    index = json.loads(download_text(config.NODE_INDEX_URL))
    latest_lts = next(v for v in index if v.get("lts"))
    return latest_lts["version"]


def adoptium_tuna_url(os_name: str, arch: str) -> str:
    """从清华 TUNA 的 Adoptium 镜像解析最新 JDK 21 下载地址（官方源降级用）。"""
    base = f"https://mirrors.tuna.tsinghua.edu.cn/Adoptium/21/jdk/{arch}/{os_name}/"
    listing = download_text(base)
    pattern = re.compile(rf"OpenJDK21U-jdk_{arch}_{os_name}_hotspot_([0-9._]+)\.(?:tar\.gz|zip)")
    found = []
    for m in pattern.finditer(listing):
        key = tuple(int(x) for x in m.group(1).replace("_", ".").split("."))
        found.append((key, m.group(0)))
    if not found:
        raise DownloadError(f"TUNA 镜像未找到 JDK 21 下载项: {base}")
    _, file_name = max(found, key=lambda kv: kv[0])
    return base + file_name
