"""固定路径、版本快照与外部 URL 常量。

目录归属是架构约束：`build/workspace/` 为自动生成（Git 忽略），
`build/loom/` 为提交的 Loom 反编译工程。
"""

from pathlib import Path

# --- 仓库内固定路径 ---------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
BUILD_DIR = REPO_ROOT / "build"
WORKSPACE = BUILD_DIR / "workspace"

ASSETS_DIR = WORKSPACE / "minecraft-assets"   # Loom 反编译源码（客户端+服务端合并，官方 mojmap）
ASSETS_JARS_DIR = ASSETS_DIR / "jars"         # 原版 client.jar / server.jar
ASSETS_LEGACY_DIR = ASSETS_DIR / "legacy"     # 从家目录迁移的旧反编译资产
PUMPKIN_DIR = WORKSPACE / "pumpkin"           # Pumpkin-MC/Pumpkin 参考实现
NEOFORGE_DIR = WORKSPACE / "neoforge"         # neoforged/neoforge 参考实现
TOOLS_DIR = WORKSPACE / ".tools"              # 本机工具链（JDK/Node 等）
JDK_DIR = TOOLS_DIR / "jdk"                   # workspace 内部署的 Temurin 21
NODE_DIR = TOOLS_DIR / "node"                 # workspace 内部署的 Node LTS

LOOM_DIR = BUILD_DIR / "loom"                 # 提交的 Loom 反编译工程
# exportSources 任务的固定产物（Loom 1.9+ 原始产物在 Gradle 用户目录缓存中）
EXPORTED_SOURCES_JAR = LOOM_DIR / "build" / "libs" / "minecraft-merged-1.21.1-sources.jar"

# --- 版本快照（固定 1.21.1 / 协议 767） --------------------------------------

MC_VERSION = "1.21.1"

# --- 外部 URL ----------------------------------------------------------------

PUMPKIN_URL = "https://github.com/Pumpkin-MC/Pumpkin"
PUMPKIN_BRANCH = "master"                 # Pumpkin 默认分支
NEOFORGE_URL = "https://github.com/neoforged/neoforge"
NEOFORGE_BRANCH = "1.21.1"                # 固定到与项目一致的 MC 1.21.1 维护分支
VERSION_MANIFEST_URL = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
ADOPTIUM_API = "https://api.adoptium.net/v3/binary/latest/21/ga/{os}/{arch}/jdk/hotspot/normal/eclipse"
NODE_INDEX_URL = "https://nodejs.org/dist/index.json"
UV_INSTALL_URL = "https://astral.sh/uv/install.sh"
RUSTUP_INIT_URL = "https://sh.rustup.rs"
PI_INSTALL_URL = "https://pi.dev/install.sh"

# --- 其它 ---------------------------------------------------------------------

# 家目录中的旧反编译资产 -> 迁移到 legacy/ 下的名字（仅本机一次性迁移，其它机器自动跳过）
LEGACY_HOME_ASSETS = [
    ("minecraft-server-1.21.1-src", "server-1.21.1-src"),
    ("minecraft-client-1.21.1-src", "client-1.21.1-src"),
]

USER_AGENT = "icbm-dev-env/1.0"
RETRY_ATTEMPTS = 3
