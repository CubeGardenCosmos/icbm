# icbm 仓库开发约定（dev 分支）

本文件继承 master 分支的提交与工程约定，并针对 dev 分支的 core/region 区域化架构、`lib/` 分层与 datarouter 通讯补充约束。master 分支仍作为协议行为与实现的参考基线。

## 项目范围

- 产品、主二进制与第一方命名空间统一为小写 `icbm`；只支持 Minecraft Java Edition 1.21.1。
- 唯一目标协议为 767（Java 1.21 与 1.21.1 共用，实现与数据一律以 1.21.1 为准）。
- 服务器采用 core + region 区域化模型（同一二进制两种模式）；区域边界隔离是有意的玩法差异，必须在文档中记录，不得在 region 内部放松原版一致性。
- [README.md](README.md) 是架构、阶段顺序与验收条件的当前来源；[docs/ipc.md](docs/ipc.md) 是 core/region 通讯协议的唯一来源。
- 反编译源码由 `build/main.py init` 生成于仓库内固定路径 `build/workspace/minecraft-assets`（客户端+服务端合并、官方 mojmap 命名；Git 忽略），只作行为分析参考；不得把 Mojang 源码、注释或资源复制进本仓库。

## 开发环境

- 所有开发人员统一使用仓库内固定路径 `build/workspace/minecraft-assets` 下的反编译源码；**禁止再依赖家目录中的反编译资产**（旧资产已由 init 迁移至该目录下的 `legacy/`）。
- 运行 `python3 build/main.py init`（推荐 `uv run build/main.py init`）完成环境就绪：同步 Git 子模块、克隆参考仓库（Pumpkin `master` / NeoForge `1.21.1` 分支，非子模块）、部署 JDK 21（Adoptium 官方源失败自动降级清华 TUNA 镜像）、Fabric Loom 反编译（`build/loom/`，官方 mojmap）。init 末尾询问是否安装基础软件（Rust 工具链、Node、pi AI 工具），并执行一次 `cargo build --release` 预热编译缓存（成败不阻断）。下载与各步骤有 cargo 风格进度显示。
- Python 工具由 uv 管理（`build/pyproject.toml`、`build/uv.lock`）；功能按分层拆分在 `build/icbm_dev_env/` 包内（入口 → 编排 → 领域功能 → 基础设施），`build/main.py` 只做 uv 自举与 CLI 分发，禁止向 `main.py` 堆逻辑。
- 所有联网操作（git/urllib/Gradle/curl/rustup）遵循 `HTTPS_PROXY`/`HTTP_PROXY` 环境变量；Gradle 步骤由脚本翻译为 JVM 代理参数。
- 环境重置与清理：`reinit`（删除 `build/workspace` 与 Loom 本地产物后重新完整部署，适合反编译结果被污染时）与 `cleanup`（`cargo clean` 清理 `target/` 腾出硬盘空间）均为破坏性操作，脚本会先请求确认。
- `build/workspace/`、`build/.venv/`、`build/loom/{.gradle,build,run}/` 整体被 Git 忽略；`build/workspace/pumpkin` 与 `build/workspace/neoforge` 是外部参考实现，只读使用，不作为子模块。
- 开发环境改动（如升级 Loom/Gradle 版本）需同步更新 `build/loom/` 与本文档。

## 参考基线（最高权重）

- **Minecraft 1.21.1 反编译源码（`build/workspace/minecraft-assets`，由 `build/main.py init` 生成）是行为一致性的最高权重参考**：协议包字段与编码、NBT 结构、Anvil/ChunkSerializer 格式、PalettedContainer 规则、注册表数据、登录/配置/Play 流转等一切可观察行为，实现时必须以源码为准频繁对照，不得凭记忆或二手资料臆测。
- 源码与本文档冲突时以源码为准，并同步修订本文档；源码与 master 分支实现冲突时，以源码为准并记录差异。
- 每个依赖源码结论的实现点应在代码注释中标注对应的源码类/方法（如 `ChunkSerializer.java`、`PalettedContainer.java`）。

## Git 原子提交

- 一个可独立验证的功能只能出现在一个提交中；不得混入构建产物、格式化噪声或无关改动。
- 提交消息使用英文类型前缀 + 中文说明，例如 `Refactor: 固定Java协议为767`、`Test: 覆盖活塞更新顺序`、`Feat: region心跳上报TPS`。
- 删除或重构前先建立测试基线；功能代码、对应测试与必要文档在同一原子提交中完成。
- 提交前运行与改动范围相称的检查，并运行 `git diff --cached --check`。
- 提交后检查工作树；若存在他人改动，必须保留、避开并在交付说明中指出。
- 不执行破坏性 Git 操作，不重写他人提交，不自动推送远端。

## 新功能草稿流程（硬约束）

**在增加任何新功能之前，必须在 `build/workspace/draft` 完成草稿**，草稿顺序固定为：

1. **先直接原样获取 Pumpkin 的做法**（`build/workspace/pumpkin`，只读参考，原样照搬对应功能/协议/状态机的实现思路与结构，不擅自改动）。
2. **再对照官方 Minecraft 服务器的做法**（`build/workspace/minecraft-assets` 反编译源码，以 1.21.1 行为为准）。
3. **对比二者不同**，用 Pumpkin 的做法修改为**兼容原版特性**的做法——即以 Pumpkin 的实现为骨架，将其与官方行为有出入的地方修正为符合原版可观察行为；**不得采用 Pumpkin 的多线程 worker 做法**（其多线程调度/分区 tick 架构不属于 icbm 的单线程 tick 所有权边界模型）。

该流程适用于所有智能体（AI 与人类开发者），是硬约束：未在 `build/workspace/draft` 完成三步草稿不得进入实现阶段；草稿结论（Pumpkin 做法、官方做法、差异与取舍）须记录在 `build/workspace/draft/<功能名>/README.md`。

## 命名与目录归属

- 第一方 Cargo package/crate 使用 `icbm-` 前缀，Rust 模块导入使用 `icbm_` 前缀。
- 目录归属是架构约束，不是建议：core 只做身份、编排、网络与全局状态；region 只做区域模拟与存档；`lib/*` 不得依赖 core/region/gameplay；gameplay 按命名空间拆分（`minecraft` / `icbm` / 后续）。
- 通讯协议变更必须同步修改 `docs/ipc.md`；新增消息 kind 必须在同一提交中更新文档与两侧实现。
- 不得机械替换 `minecraft:*`、原版 NBT 键、协议字段、Mojang/Yarn 类型名或第三方项目名。

## 代码质量：禁止面条代码

- **分层单向依赖**：core → region → lib/{anvil,terrain-gen,mc-767,datarouter}；gameplay 只消费注册表与指令接口。任何反向或横向跨层引用（如 region 直接依赖 core 类型）视为架构违规。
- **单一职责**：模块/结构体职责单一；禁止“上帝对象”。Region、Player、Chunk 等核心类型的字段与行为按域拆分（如区块存储、tick、网络序列化分开）。
- **小函数**：优先短函数与明确命名；避免超过一屏的嵌套分支/循环。复杂流程用中间结构体或子函数拆分，不为“少写几行”牺牲可读性。
- **显式状态机**：region 生命周期（启动/模拟/沉睡/卸载）、连接状态（握手/状态/登录/配置/Play）必须用显式状态机表达，禁止靠散落的布尔标志推断状态。
- **无全局可变状态**：禁止 `static mut` 与隐藏的全局可变容器；跨线程数据必须经所有权边界或明确的锁/通道传递。
- **错误处理**：不以 `unwrap`、静默 fallback 或吞错方式掩盖协议、存档或注册表不一致；错误要有类型与上下文，向调用方显式传播。
- **避免重复实现**：同一格式（NBT、VarInt、区块 palette）只在一处实现；复用 `lib/mc-767` 与 `lib/anvil`，不在 core/region 内另写副本。

## 协议、存档与数据约束

- 新增协议代码只能针对 Java 1.21.1/协议 767，不得引入运行时版本分支。
- 固定版本快照：协议 767、DataVersion 3955、资源包格式 34、数据包格式 48；存档各层不得混用其它版本号。
- 包 ID、字段布局、registry、tag、tracked data、data component、实体 metadata 必须来自同一 1.21.1 基线。
- 非 767 客户端确定性拒绝。
- 存档（`lib/anvil`）必须与 vanilla 1.21.1 互读：.mca 头部/扇区/压缩（含 LZ4 与 `.mcc` 外部流）、chunk NBT 结构以 `RegionFile.java`/`ChunkSerializer.java` 为对照；压缩与序列化必须有 golden/round-trip 测试。
- 修改生成数据时优先修改生成器与固定输入再重新生成，不手工修补生成文件。

## 原版一致性与运行时约束

- 以可观察行为为移植目标：状态变化、数据包、事件顺序、存档结果、命令反馈与 1.21.1 对齐；参考反编译源码（官方 mojmap 命名）分析行为，但用 Rust 独立实现并以测试证明。
- **安全聊天（secure chat）永不支持**：所有场景下 `enforcesSecureChat` 必须为 false；不实现签名聊天、消息头部、会话 UUID（chat session）、`minecraft:chat` 通道签名路径。未来任何涉及聊天/命令的改动不得引入安全聊天假设。
- 每项“原版一致”结论关联单元测试、golden test、快照或可复现实验；有意差异（区域边界截断、迁移、Y > 400 虚拟区域）记录到 parity matrix。
- 游戏状态只可在模拟/tick 所有权边界内变更；网络、认证、文件 I/O、压缩、哈希不得阻塞 tick 热路径。
- 随机行为使用正确的世界/实体随机源，保证固定种子测试可复现。

## 验证要求

- 文档或仓库约定改动：至少 `git diff --check` 与 `git diff --cached --check`。
- Rust 改动：至少受影响 crate 的格式检查、Clippy 与测试；公共 API 或跨 crate 改动运行全工作区检查。
- 协议改动：协议 767 golden tests + 连接/状态机集成测试。
- 存档改动：`lib/anvil` 必须含 .mca/chunk NBT 的 round-trip 与 vanilla 生成的 golden 文件测试。
- 依赖清理后运行未使用依赖检查；不提交 `target/`、Loom 缓存、反编译源码、临时下载或本机专用配置。
