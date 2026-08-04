# icbm 进程间通讯协议（datarouter 二进制帧）

core 与 region 通过 `lib/datarouter`（共享内存传输层）通讯。应用层不使用业务
JSON / TLV 上下文，直接使用紧凑二进制帧，最大程度降低每消息开销。

## 传输

- 共享内存由 `datarouter-switch` 创建；datarouter 子模块把 switch 二进制
  `include_bytes!` 嵌入 icbm 可执行文件，**core 启动时自动拉起 switch**
  （`SwitchHandle`，memfd 内存执行），不再需要预先运行外部 switch。
- 简单鉴权（可选）：设置环境变量 `ICBM_DATAROUTER_TOKEN` 后，core 以
  `--token` 启动 switch（控制面只存 FNV-1a 指纹），core 与 region 连接时携带
  同一 token；未设置则不启用（shm 权限 0600 已限制为同用户）。
- 单帧容量：ring 为 512KiB，`MAX_APP_PAYLOAD` = 512KiB − 16B；≤512KiB 消息
  单帧传输，不再强制分块（旧 60KiB 上限与 4MiB 重编约定已废弃）。
- 接收轮询约定：`Frontend::recv` 会把超时 0 归一化为 `recv_timeout_ms`
  （默认 3000ms），**禁止用 `recv(0)` 实现非阻塞**；region 排空用 `recv(1)`。
- core 注册服务名：`icbm.core`
- region 注册服务名：`icbm.region.<id>`，其中 `<id>` 为紧凑区域标识（见下）。
- 消息类型使用 datarouter `MSG_CONTROL`（0x8B），payload 放入 body；var_data 留空。

## 区域标识

`<id>` 采用 `维度字母 + 区域区块坐标`（LE 编码为 ASCII）：

| 维度     | 字母 | 区域大小（区块） | 标识示例        |
|----------|------|------------------|-----------------|
| 主世界   | `o`  | 32 × 32          | `o_0_0`         |
| 地狱     | `n`  | 4 × 4            | `n_0_0`         |
| 末地     | `e`  | 不拆分（全局）   | `e`             |

主世界与地狱区域坐标对齐：主世界区域 `(x, z)` 对应的地狱区域块坐标为
`(x << 2, z << 2)`（32 → 4，即 1:8）。一个 region 进程同时拥有
`o_<x>_<z>` 与 `n_<x>_<z>` 两个维度面。

### 存档文件归属

region 进程通过 `lib/anvil` 独占读写其 `.mca` 文件并持有文件锁：

| 维度面   | 路径                                     | 文件                        |
|----------|------------------------------------------|-----------------------------|
| 主世界   | `world/region/`                          | `r.<rx>.<rz>.mca`（32×32）   |
| 地狱     | `world/DIM-1/region/`                    | `r.<rx>.<rz>.mca`（4×4）     |
| 末地     | `world/DIM1/region/`                     | 全局单文件                  |

## 帧格式

```
offset 0: u8   kind
offset 1: 载荷（按 kind 定义，全部小端）
```

| kind  | 名称               | 方向      | 载荷                                                                 |
|-------|--------------------|-----------|----------------------------------------------------------------------|
| 0x01  | REGION_REGISTER    | R → C     | `[u8 dim][i32 rx][i32 rz][u32 pid]`                                  |
| 0x81  | REGISTER_ACK       | C → R     | `[u8 dim][i32 rx][i32 rz][u8 ok][i32 entity_id_base]`（实体 ID 基址） |
| 0x02  | REGION_HEARTBEAT   | R → C     | `[f32 tps][u8 sleeping][u32 loaded_chunks][u64 tick]`                |
| 0x03  | REGION_UNLOAD      | C → R     | `[u8 dim][i32 rx][i32 rz]`（收到后 region 正常退出）                  |
| 0x04  | PLAYER_ENTER       | C → R     | `[u128 uuid][f64 x][f64 y][f64 z][f32 yaw][f32 pitch]`               |
| 0x05  | PLAYER_LEAVE       | C → R     | `[u128 uuid]`                                                        |
| 0x06  | CHUNK_REQUEST      | C → R     | `[u8 dim][i32 cx][i32 cz][i32 view_radius]`                          |
| 0x07  | CHUNK_DATA         | R → C     | `[u8 dim][i32 cx][i32 cz][u32 len][bytes：区块网络数据]`              |
| 0x08  | REGION_STATS       | C → R     | 空载荷；region 立即回 0x09                                           |
| 0x09  | REGION_STATS_RESP  | R → C     | `[f32 tps][f32 avg_tps_ms][u32 tick_count][u8 sleeping][u32 loaded_chunks]` |
| 0x0A  | ENTITY_DATA        | R → C     | `[u8 dim][i32 rx][i32 rz][i32 packet_id][bytes：clientbound 包载荷]`；core 转发给该区域玩家 |
| 0x0B  | BLOCK_UPDATE       | C → R     | `[u8 dim][i32 x][i32 y][i32 z][i32 state_id]`（`/setblock`/`/fill`）       |
| 0x0D  | BLOCK_QUERY        | C → R     | `[u8 dim][i32 x][i32 y][i32 z]`（方块查询 → 0x8D）                         |
| 0x8D  | BLOCK_QUERY_RESP   | R → C     | `[u8 dim][i32 x][i32 y][i32 z][i32 state_id]`                              |
| 0x0E  | GAMERULE_UPDATE    | C → R     | `[u8 名长][名][u8 值长][值]`                                                |
| 0x0F  | PLAYER_MOVE        | C → R     | `[u128 uuid][f64 x][f64 y][f64 z]`（玩家位置同步，敌对生物追踪；core 节流约 4 次/秒） |
| 0x10  | TIME_UPDATE        | C → R     | `[i64 tick][u8 add]`（`/time set/add`；add=1 表示累加）                     |
| 0x11  | BLOCK_NBT_GET      | C → R     | `[u8 dim][i32 x][i32 y][i32 z]`（方块实体 NBT 读取 → 0x91）                 |
| 0x91  | BLOCK_NBT_RESP     | R → C     | `[u8 dim][i32 x][i32 y][i32 z][u8 has][NBT]`（`/data get block`）           |
| 0x12  | BLOCK_NBT_SET      | C → R     | `[u8 dim][i32 x][i32 y][i32 z][NBT]`（`/data merge/remove block`）          |
| 0x13  | COMMAND_EXECUTE    | R → C     | `[u8 长][命令]`（命令方块触发，core 以控制台身份执行）                      |
| 0x14  | BLOCK_RAYCAST      | C → R     | `[f64 ox][f64 oy][f64 oz][f64 dx][f64 dy][f64 dz][f64 max]`（射线检测 → 0x94；末影珍珠落点） |
| 0x94  | BLOCK_RAYCAST_RESP | R → C     | `[u8 hit][f64 x][f64 y][f64 z]`（命中方块坐标；未命中 hit=0）               |
| 0x15  | WEATHER_UPDATE     | C → R     | `[u8 raining][u8 thundering][f32 rain_gradient][f32 thunder_gradient]`（core 全局天气同步） |
| 0x16  | SURFACE_QUERY      | C → R     | `[u8 dim][i32 x][i32 z]`（地表高度查询 → 0x96；新玩家出生点适配）           |
| 0x96  | SURFACE_RESP       | R → C     | `[i32 y]`（最高非空气方块 y+1）                                             |
| 0x17  | PORTAL_CREATE      | C → R     | `[u8 dim][i32 x][i32 y][i32 z]`（维度切换目的端生成返回传送门；以玩家落点周边建 4×5 黑曜石框架并填充传送门方块，广播 `CBlockUpdate`） |
| 0x18  | WORKER_REGISTER    | W → C     | `[u32 pid][u8 cpu]`（worker 进程注册；core 记入 WorkerRegistry）             |
| 0x19  | WORKER_ASSIGN      | C → W     | `[i32 rx][i32 rz]`（core 分配 region 给 worker；worker 内启动 region 模拟线程） |
| 0x1A  | WORKER_RELEASE     | C → W     | `[i32 rx][i32 rz]`（core 释放 region；worker 内停止 region 线程并 flush，进程存活） |

## worker 进程池

- core 启动时拉起 **`cpu_count - 2` 个常驻 worker 进程**（`icbm --worker <id>
  --cpu <core>`，至少 1 个；`ICBM_WORKERS` 环境变量可覆盖），每个 worker
  注册 `icbm.worker.<id>` 并 `sched_setaffinity` 绑定到除 core 之外的一个
  CPU 核心（core 占核心 0，worker i 绑核心 i+1）。
- region 由 core 经 `WORKER_ASSIGN` 分配给**负载最低**的 worker（region
  合并：活跃 region 数 > worker 数时多个 region 共享一个 worker 进程）；
  worker 内每 region 一个独立线程运行 `RegionService`（复用单 region 主循环：
  注册 `icbm.region.<id>`、20 TPS tick、IPC 处理），region 之间互不干扰，
  各自持有独立 datarouter 连接与 `ChunkStore`——符合“每 region 单线程 tick
  所有权边界”（进程只是容器 + CPU 绑核单位，不引入进程内并行模拟）。
- region 卸载 = core 发 `WORKER_RELEASE` → worker 内停止 region 线程并
  flush（进程存活），**不再 kill 进程**，消除“flush 超时被杀”与
  “spawn/remove 竞态注册被拒”故障。
- core 关停：向各 region 发 `REGION_UNLOAD`（region 线程 flush 后退出），
  随后 kill 全部 worker 进程（容器）。

## 生命周期

1. core 启动 → `SwitchHandle` 拉起嵌入式 switch（`--allow-any --ttl-ms 5000
   --max-ports 8 [--token …]`）→ 连接并注册 `icbm.core`；读取配置，拉起
   **worker 进程池**（`cpu_count - 2` 个常驻 worker，各绑一个 CPU 核心），
   为出生点区域经 `WORKER_ASSIGN` 分配第一个 region。
2. worker 启动 → 连接 datarouter → 注册 `icbm.worker.<id>` → 发送
   `WORKER_REGISTER`（含 PID 与绑核）。core 收到 `WORKER_ASSIGN` 后在其
   进程内启动 region 模拟线程（注册 `icbm.region.<id>`、发 `REGION_REGISTER`）。
3. core 回复 `REGISTER_ACK`（含分配的实体 ID 基址，区域内实体 ID ∈
   `[base, base+999]`）后，region 进入正常模拟；每 1 秒发送一次
   `REGION_HEARTBEAT`（含 TPS 与沉睡状态）。
4. 玩家进出：core 发送 `PLAYER_ENTER` / `PLAYER_LEAVE`；region 在无玩家时进入沉睡（停止 tick），持续 30 秒后由 core 发送 `WORKER_RELEASE`（worker 内停止 region 线程并 flush，进程存活）卸载。
5. 区块数据：core 收到客户端进入/跨区时，向视野覆盖的**全部** region 发
   `CHUNK_REQUEST`（视野方块 [cx±r, cz±r] 最多覆盖 4 个主世界 region）；region
   只回其拥有的区块（主世界 32×32 裁剪，不再把邻区区块以 flat 生成覆盖），
   沉睡 region 保持在线继续服务区块请求；**已卸载** region 的 .mca 由 core
   只读打开（`RegionFile::open_readonly`，共享锁）直接构建区块数据转发，
   区块缺失时用 flat 生成兜底。改动经 `lib/anvil` 写回 `.mca`。
6. 实体：region 模拟 Creeper 敌对生物（夜晚生成、追逐/引爆），经 `ENTITY_DATA` 把
   `ADD_ENTITY`/`MOVE_ENTITY_POS` 等 clientbound 包发给 core，core 转发给位于
   该区域的玩家；region 卸载时 core 按实体 ID 段发 `REMOVE_ENTITIES` 清理客户端实体。
7. 方块交互：`/setblock`/`/fill`（BLOCK_UPDATE）、方块查询（BLOCK_QUERY，传送门/屏障检测）、
   方块实体 NBT（BLOCK_NBT_GET/SET，`/data` 与命令方块 Command）、末影珍珠落点
   （BLOCK_RAYCAST）、地表高度（SURFACE_QUERY，新玩家出生点适配）、维度切换目的端
   生成返回传送门（PORTAL_CREATE）均经 IPC 到 region；命令方块被红石供电后经
   COMMAND_EXECUTE 交回 core 以控制台身份执行。
8. **全局时间/天气由 core 管理**：receiver 线程每秒推进 core 全局时间（`doDaylightCycle`）
   与天气（`doWeatherCycle`）并广播 `TIME_UPDATE`/`WEATHER_UPDATE` 给所有 region；
   region 不再各自推进，只把收到的状态广播给各自玩家（跨区域一致）。`/time`、
   `/weather` 指令直接改 core 全局状态后广播。
9. switch 异常退出/扩容重启时，既有 Frontend 在下一次 touch/send 时自动重注册；region 侧由 `send_retry` 对 BUSY/FULL/AGAIN 短退避重试。

## 错误处理

- 帧长度不符 / 未知 kind：接收方丢弃并记录计数。
- 心跳超时（默认 5 秒）：core 视 region 失联，回收其租约并重启 region 进程。
- region 非正常退出：core 从注册表摘除并告警，不自动重启（避免风暴）；由显式操作或玩家需求重新拉起。
- switch 残留：交互式 `Ctrl+C`（SIGINT 到整个前台进程组）会同时终止 core 与
  switch；若单独 `kill <core_pid>`（SIGTERM），switch 会成为孤儿进程并持有
  共享内存，需 `pkill -f datarouter-switch` 清理（下一次 core 启动会探测到
  同名 switch 并复用，或按 `AlreadyRunning` 处理）。
