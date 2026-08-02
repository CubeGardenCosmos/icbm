# icbm 仓库开发约定（dev 分支）

本文件继承 master 分支的提交与工程约定，并针对 dev 分支的 core/region 区域化架构、`lib/` 分层与 datarouter 通讯补充约束。master 分支仍作为协议行为与实现的参考基线。

## 项目范围

- 产品、主二进制与第一方命名空间统一为小写 `icbm`；只支持 Minecraft Java Edition 1.21.1。
- 唯一目标协议为 767（Java 1.21 与 1.21.1 共用，实现与数据一律以 1.21.1 为准）。
- 服务器采用 core + region 区域化模型（同一二进制两种模式）；区域边界隔离是有意的玩法差异，必须在文档中记录，不得在 region 内部放松原版一致性。
- [README.md](README.md) 是架构、阶段顺序与验收条件的当前来源；[docs/ipc.md](docs/ipc.md) 是 core/region 通讯协议的唯一来源。
- Loom 反编译源码位于仓库外（`~/minecraft-server-1.21.1-src`），只作行为分析参考；不得把 Mojang 源码、注释或资源复制进本仓库。

## Git 原子提交

- 一个可独立验证的功能只能出现在一个提交中；不得混入构建产物、格式化噪声或无关改动。
- 提交消息使用英文类型前缀 + 中文说明，例如 `Refactor: 固定Java协议为767`、`Test: 覆盖活塞更新顺序`、`Feat: region心跳上报TPS`。
- 删除或重构前先建立测试基线；功能代码、对应测试与必要文档在同一原子提交中完成。
- 提交前运行与改动范围相称的检查，并运行 `git diff --cached --check`。
- 提交后检查工作树；若存在他人改动，必须保留、避开并在交付说明中指出。
- 不执行破坏性 Git 操作，不重写他人提交，不自动推送远端。

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

- 以可观察行为为移植目标：状态变化、数据包、事件顺序、存档结果、命令反馈与 1.21.1 对齐；参考 Yarn 源码分析行为，但用 Rust 独立实现并以测试证明。
- 每项“原版一致”结论关联单元测试、golden test、快照或可复现实验；有意差异（区域边界截断、迁移、Y > 400 虚拟区域）记录到 parity matrix。
- 游戏状态只可在模拟/tick 所有权边界内变更；网络、认证、文件 I/O、压缩、哈希不得阻塞 tick 热路径。
- 随机行为使用正确的世界/实体随机源，保证固定种子测试可复现。

## 验证要求

- 文档或仓库约定改动：至少 `git diff --check` 与 `git diff --cached --check`。
- Rust 改动：至少受影响 crate 的格式检查、Clippy 与测试；公共 API 或跨 crate 改动运行全工作区检查。
- 协议改动：协议 767 golden tests + 连接/状态机集成测试。
- 存档改动：`lib/anvil` 必须含 .mca/chunk NBT 的 round-trip 与 vanilla 生成的 golden 文件测试。
- 依赖清理后运行未使用依赖检查；不提交 `target/`、Loom 缓存、反编译源码、临时下载或本机专用配置。
