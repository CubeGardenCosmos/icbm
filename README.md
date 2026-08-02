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
- 玩家所在 region 为中心，其 **9 × 9** 范围内未生成地图的 region 会拉起子进程进行多线程生成。
- 默认模拟距离 8，最大可视距离 64；允许读取所在 region 外的地形。
- 玩家离开且 region 内无玩家时：传送处于跟随状态的伴侣动物，region 标记为**沉睡**（停止 tick 更新）。
- region 沉睡超过 30 秒后**卸载**（停止 region 进程）；沉睡的 region 允许破坏方块，但除锁定玩家的 BOSS、掉落物之外的实体停止更新。
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

- `lib/datarouter` 为 Git 子模块（`git@github.com:Creeper23456/datarouter.git`），提供共享内存传输与 Rust Frontend。
- 服务命名：core 注册为 `icbm.core`；region 注册为 `icbm.region.<id>`。
- **channel 容量**：默认 512KiB（`DATAROUTER_CHANNEL_SIZE`）。Phase 1 区块一次下发约 4MB，部署时需以 `DATAROUTER_CHANNEL_SIZE=4194304` 重编 switch（`-DDATAROUTER_CHANNEL_SIZE=4194304`）与 Rust 侧（构建时环境变量），两侧不一致会因布局校验失败而无法连接。
- 应用层约定**无上下文开销的二进制协议**（见 `docs/ipc.md`）：消息体为紧凑二进制帧，不使用业务 JSON/TLV 上下文。
- 消息类型：注册、心跳（TPS/沉睡状态）、玩家进出、区块请求与数据、卸载指令等。

## 第一阶段范围（Phase 1）

- [ ] 地形生成器：主世界、地狱、末地统一为 flat 超平坦。
- [ ] `lib/anvil`：1.21.1 Anvil 存档格式——.mca region 文件（8KiB 头部/扇区分配/压缩编解码，含 LZ4 与外部 .mcc 流）、chunk NBT 编解码（sections/palette/heightmap/block_entities/entities）、level.dat 与 playerdata。
- [ ] core–region 分型基本功能：region 拉起/注册/心跳/沉睡/卸载、9 × 9 预生成；region 通过 `lib/anvil` 读写并锁住其 `.mca` 存档文件。
- [ ] `gameplay/minecraft`：方块、生物、声音注册；`gamerule` 与 `gamemode` 指令。
- [ ] `/bmtool` 工具：当前仅 `/bmtool tps`，输出 CPU 负载、CPU 核心数、活跃 Region 数、沉睡 Region 数、当前 region TPS、平均 region TPS。
- [ ] 控制台：参考 master 分支（stdin 线程 + 命令分发）。
- [ ] 默认 icon 与 MOTD：参考 master 分支（状态查询响应，内置 favicon）。
- [ ] `lib/mc-767`：协议 767 编解码（握手/状态/登录/配置/Play 最小子集）+ NBT。
- [ ] datarouter 接入：core/region 通过共享内存通讯，二进制协议落地。

## 参考基线

- Minecraft 1.21.1 反编译源码（Yarn `1.21.1+build.3:v2`）：`~/minecraft-server-1.21.1-src`，共 4,064 个 Java 文件，仅作行为分析参考。
- 成熟 tokio Minecraft 协议实现：本仓库 `master` 分支，作为控制台、默认 icon/MOTD、协议行为等实现的参考。
- 协议与版本常量（1.21.1 / 767 / DataVersion 3955 等）以 `net.minecraft.SharedConstants` 与 master 分支 `util/src/version.rs` 为准。

## 构建与运行

```bash
# 检出子模块
git submodule update --init

# 构建
cargo build --release

# 运行 core（默认模式）
./target/release/icbm
```

> 依赖 datarouter-switch 进程先于 core 启动；部署细节见 `docs/ipc.md`。

## 开发约定

- 提交信息使用英文，前缀（`Feat:` / `Fix:` / `Config:` / `Refactor:` / `Docs:` / `Test:`）参考 master 分支历史。
- 任何偏离原版可观察行为的设计（如区域边界截断、迁移、Y > 400 虚拟区域）必须记录在文档中。
- 不提交 Loom 反编译源码；不引入通用插件 ABI。
