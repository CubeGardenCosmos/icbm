# icbm

## 用途

`icbm` 用于在固定、可复现的 Minecraft Java Edition 1.21.1 环境中开展以下工作：

- 为 Cosmos 项目的 Shiroitori 智能体提供可感知、可移动和可共同活动的三维实验场；
- 与 BlockMember 的合作中验证 Minecraft 服务端行为复现、基础设施和运营方案；
- 评估区域化游戏服务的调度、隔离与成本，并沉淀可审计的工程协作流程。

## 定位与约束

- 仅兼容 Minecraft Java Edition **1.21.1**（网络协议 **767**），不做多版本兼容；1.21 与 1.21.1 共用协议 767，服务端按 1.21.1 固定行为基线并拒绝无法确认的差异。
- 以 `core + region` 区域化微服务模型取代传统全局多线程 tick；区域边界隔离是有意的玩法差异，必须在原版一致性矩阵中单独记录与测试。
- 工程目标是在"多核充足、单核相对薄弱"的基础设施上提升吞吐、降低单位游戏服务成本；这不放宽 region 内部的原版一致性要求。
- 严格闭源项目：参考源码（Loom 反编译）只作为行为分析参考，不提交到本仓库。

## 架构总览

服务器由一个二进制、两种运行模式组成：

```
icbm                    # 同一二进制
├── core 模式           # 默认；服务器主进程
└── region 模式         # 子进程（--region <参数>）；区域模拟服务
```

### core（核心服务）

core 是服务器运行的主进程，负责：

- 初始化 datarouter（共享内存传输层），注册 `icbm.core` 服务；
- 读取服务器配置，启动第一个 Region（出生点所在区域）；
- 配方注册；Y > 400 时的虚拟区域管理（该高度活动不加载任何 region）；
- 玩家以及玩家绑定的实体（驯服的动物）的权威数据、权限与聊天；
- Region 生命周期管理：注册、心跳、沉睡、卸载、按需拉起与预生成。

### region（区域模拟服务）

region 是服务器的子进程，作为 datarouter 中的独立服务（`icbm.region.<id>`）加入共享内存池，负责其注册存档文件所覆盖区域内的主要事件：

- 主世界：持有 **32 × 32 区块**（Y < 400），并锁住对应存档区块文件；
- 地狱：持有对应主世界区域按 1:8 缩放的 **4 × 4 区块**；
- 一个 region 同时负责该区域内的地狱与主世界事件；
- 末地是特殊维度：按传统 Minecraft 服务器方式运行，**不拆分 region**，没有玩家时卸载。

### 边境与玩家行为

- 玩家跨越 region 边界时速度不得超过 2 方块/秒；乘船跨越会被减速。
- 装备鞘翅高速接近边境时"感受到动能"并死亡；空中高速靠近且剩余 50 方块时，Action Bar 提示："你当前正在边境，请爬升高度。"
- 玩家所在 region 为中心，其 **3 × 3** 范围内未生成地图的 region 会拉起子进程进行
  地形生成（README 原计划 9 × 9，但 datarouter 端口上限 `DATAROUTER_MAX_PORTS`=64，
  9×9=81 超出；更远处由移动触发的区块请求按需拉起）。
- 默认模拟距离 8，最大可视距离 64；允许读取所在 region 外的地形。
- 玩家移动时，core 按 vanilla 规则在玩家走出已发区块缓存中心 `view/2` 时重新请求
  视野区块并更新 `SetChunkCacheCenter`，跨 region 时先拉起/复用新 region 再请求，
  避免落入未加载地形。
- 玩家离开且 region 内无玩家时：传送处于跟随状态的伴侣动物，region 标记为**沉睡**（停止 tick 更新）。
- region 沉睡超过 30 秒后**卸载**（停止 region 进程）；卸载时 core 向在线玩家发
  `REMOVE_ENTITIES` 清理该区域的实体。沉睡的 region 允许破坏方块，但除锁定玩家的
  BOSS、掉落物之外的实体停止更新。
- **Creeper 敌对生物（region 存活信号）**：夜晚（世界时间 13000..23000）以概率在
  区域内生成 Creeper（上限 12 只，`doMobSpawning` 关闭时禁止），使用原版 AI：
  追踪最近玩家（16 格内）、3 格内点燃引信（30 tick 后爆炸，半径 3）；其存在本身
  也是验证 region 是否仍在运行/已卸载的视觉信号——region 卸载后 Creeper 随之消失。
- Y > 400 活动时不加载任何 region；被卸载的 region 是只读的，无法进行任何交互。

## 目录结构

```
core/                 # 服务器主进程（icbm 二进制 + core 模式）
region/               # 区域模拟服务（同一二进制的 region 模式）
gameplay/             # 游玩主要内容，按命名空间拆分
  minecraft/          #   Minecraft 主要游戏内容：方块、合成配方、成就注册表、生物、指令
  icbm/               #   服务器特定协议：部分指令（/bmtool 等）与后续内容
lib/                  # 可复用底层库
  anvil/              #   1.21.1 Anvil 存档格式：.mca region 文件、chunk NBT、level.dat、playerdata
  datarouter/         #   子模块：进程间通讯（共享内存传输层）
  i18n/               #   格式化国际化文本（预留）
  mc-767/             #   1.21.1 网络协议 Rust 实现，含 NBT
  terrain-gen/        #   地形生成器（Minecraft 及后续追加）
```

### 存档布局（Anvil）

一个 region 进程的主世界面恰好对应一个 `.mca` region 文件（32 × 32 区块），地狱面对应
另一个 `.mca`（4 × 4 区块）。尺寸与坐标对齐见 `docs/ipc.md`。

```
world/
├── level.dat                  # 世界根数据（DataVersion 3955）
├── level.dat_old
├── region/                    # 主世界 region 文件 r.<rx>.<rz>.mca
├── DIM-1/region/              # 地狱
├── DIM1/region/               # 末地（不拆分，全局单文件）
└── playerdata/<uuid>.dat      # 玩家数据
```

## 进程间通讯（datarouter）

- `lib/datarouter` 为 Git 子模块（`git@github.com:Creeper23456/datarouter.git`），提供共享内存传输与 Rust Frontend；switch 二进制嵌入 icbm 可执行文件，core 启动时自动拉起（无需外部进程）。可选鉴权：设置环境变量 `ICBM_DATAROUTER_TOKEN`。
- 服务命名：core 注册为 `icbm.core`；region 注册为 `icbm.region.<id>`。
- **单帧容量**：ring 为 512KiB（`MAX_APP_PAYLOAD` = 512KiB − 16B），≤512KiB 消息单帧传输，不再分块；旧 60KiB 上限与 4MiB 重编约定（`DATAROUTER_CHANNEL_SIZE=4194304`）已废弃。
- 应用层约定**无上下文开销的二进制协议**（见 `docs/ipc.md`）：消息体为紧凑二进制帧，不使用业务 JSON/TLV 上下文。
- 消息类型：注册、心跳（TPS/沉睡状态）、玩家进出、区块请求与数据、卸载指令等。

## 第三阶段范围（Phase 3）

- [x] 进服信息：玩家进入游戏后打印所在坐标、IP、UUID、维度与语言（`ClientInformation` 的 locale；语言变更仅在 debug 日志输出，进服时输出一次）。
- [x] 红石高级特性：BUD（活塞准连接供电）与活塞（伸出推块/粘性拉回）；珊瑚与 TNT 刷取（推动时末端复制一份）。
- [x] 命令方块与 Minecraft 原版指令：`fill`、`setblock` 及具备 NBT 编辑能力的指令子项（`/data get|merge|remove block`）；游戏内指令补全。
- [x] 天气系统（雨/雷暴梯度与 `GAME_EVENT` 广播）与农作物生长系统（小麦等随机 tick 生长）；友好生物随机生成系统。
- [x] 敌对生物及敌对生物 AI：苦力怕使用原版生成机制（仅夜晚）和 AI（追逐/3 格引信/爆炸）；不含 boss，不含幻翼。
- [x] 功能性实体与方块：亮度方块（`light`）、屏障、命令方块。
- [x] 鞘翅/屏障行为：创造模式玩家飞行不能穿过屏障（位置回退）；乘船（`MOVE_VEHICLE`）且水面上的玩家可以。
- [x] 末影珍珠改为计算落点后直接传送（region 射线检测，无实体投掷、不加载区块）。
- [x] 超平坦从洞穴更新后的最低建筑点（Y=-64）开始生成。

> 备注：鞘翅用皮革修补需要铁砧/锻造台系统，当前阶段未实现（无合成/铁砧）；
> 仅保证鞘翅物品与实体行为可用。

## 参考基线

- Minecraft 1.21.1 反编译源码（Fabric Loom 生成，客户端+服务端合并、官方 mojmap 命名）：`build/workspace/minecraft-assets`（由 `python3 build/main.py init` 生成，Git 忽略），仅作行为分析参考。
- 成熟 tokio Minecraft 协议实现：本仓库 `master` 分支，作为控制台、默认 icon/MOTD、协议行为等实现的参考。
- 协议与版本常量（1.21.1 / 767 / DataVersion 3955 等）以 `net.minecraft.SharedConstants` 与 master 分支 `util/src/version.rs` 为准。

## 构建与运行

### 开发环境准备

```bash
# 一键就绪：同步子模块 + 克隆 Pumpkin/NeoForge 参考 + 部署 JDK 21 + Loom 反编译
uv run build/main.py init        # 推荐（uv 缺失时脚本自动安装并改用 uv 运行）
# 或：python3 build/main.py init
```

- 反编译源码固定位于 `build/workspace/minecraft-assets`；Pumpkin/NeoForge 参考实现位于
  `build/workspace/pumpkin`、`build/workspace/neoforge`（非子模块）。整个 `build/workspace/` 被 Git 忽略。
- init 末尾会询问是否安装基础软件（Rust 工具链 cargo/rustc/rustfmt、Node、pi）；非交互环境可用 `--with-tools` 跳过询问。
- init 最后会执行一次 `cargo build --release` 预热编译缓存（无论成败都会继续，日志提示结果）；下载/分步显示为 cargo 风格进度条。
- 代理：所有联网操作遵循 `HTTPS_PROXY`/`HTTP_PROXY` 环境变量（Gradle 步骤自动翻译为 JVM 代理参数）；JDK 下载在官方源失败时自动降级清华 TUNA 镜像。
- Python 工具由 uv 管理，逻辑分层在 `build/icbm_dev_env/` 包内。
- 子命令：`submodules` / `clones` / `jdk` / `decompile` / `migrate-legacy` / `tools` / `rustfmt` / `status` / `clean`。

> **正版验证默认开启**：服务端默认按正版模式登录（RSA 加密 + Mojang session
> server 校验，获取真实 UUID 以加载皮肤）。离线/内网环境可设
> `ICBM_ONLINE_MODE=offline` 关闭；认证服务器地址可用 `ICBM_AUTH_URL` 覆盖
> （占位符 `{username}`/`{server_hash}`，支持 Drasl/Blessing Skin 等第三方），
> 超时用 `ICBM_AUTH_TIMEOUT_MS`（默认 8000）。认证失败时客户端显示 vanilla
> 翻译文案（authservers down 等）。玩家游戏模式与离线位置保存在
> `world/playerdata/<uuid>.dat`，重新进入自动恢复；新玩家出生在真实地表之上。
> 全局时间与天气由 core 统一管理（`/time`、`/weather` 指令），各 region 一致。

```bash
# 检出子模块
git submodule update --init

# 构建
cargo build --release

# 运行 core（默认模式）
./target/release/icbm
```

命令行参数：

- `--world <目录>`：世界目录（默认 `world`）；
- `--port <端口>`：监听端口（默认 25565）；
- `--shm <名称>`：datarouter 共享内存对象名（默认 `/icbm`）；
- `--log-level <off|error|warn|info|debug|trace>`：日志级别，默认 `info`；
  TRACE/DEBUG/INFO/WARN/ERROR 严格分层，region 子进程继承同一级别。

> 依赖 datarouter-switch 进程先于 core 启动；部署细节见 `docs/ipc.md`。

### 单文件可移植 release（任意 Linux）

`scripts/build-release.sh` 构建**静态链接**的 `x86_64-unknown-linux-musl` 单文件二进制，
可在任意 Linux 发行版直接运行（无 glibc 版本依赖、无外部命令依赖）：

```bash
./scripts/build-release.sh   # 产物：target/x86_64-unknown-linux-musl/release/icbm
```

运行所需资源全部内嵌：favicon（`assets/icbmlogo.png`，`include_bytes!`）、
`datarouter-switch` 二进制（`lib/datarouter` build.rs 编译后 `include_bytes!`，启动时
经 Linux `memfd` 内存执行、零落盘）；region 子进程由同一二进制 `--region` 拉起。
构建依赖 `rustup target add x86_64-unknown-linux-musl` 与 `musl-gcc`
（Fedora `musl-gcc` / Debian `musl-tools`）。

## 开发约定

- 提交信息使用英文，前缀（`Feat:` / `Fix:` / `Config:` / `Refactor:` / `Docs:` / `Test:`）参考 master 分支历史。
- 任何偏离原版可观察行为的设计（如区域边界截断、迁移、Y > 400 虚拟区域）必须记录在文档中。
- 不提交 Loom 反编译源码；不引入通用插件 ABI。
