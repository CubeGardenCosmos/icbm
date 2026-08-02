# icbm 进程间通讯协议（datarouter 二进制帧）

core 与 region 通过 `lib/datarouter`（共享内存传输层）通讯。应用层不使用业务
JSON / TLV 上下文，直接使用紧凑二进制帧，最大程度降低每消息开销。

## 传输

- 共享内存由 `datarouter-switch` 创建，core 启动前必须先运行 switch。
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
| 0x81  | REGISTER_ACK       | C → R     | `[u8 dim][i32 rx][i32 rz][u8 ok]`                                    |
| 0x02  | REGION_HEARTBEAT   | R → C     | `[f32 tps][u8 sleeping][u32 loaded_chunks][u64 tick]`                |
| 0x03  | REGION_UNLOAD      | C → R     | `[u8 dim][i32 rx][i32 rz]`（收到后 region 正常退出）                  |
| 0x04  | PLAYER_ENTER       | C → R     | `[u128 uuid][f64 x][f64 y][f64 z][f32 yaw][f32 pitch]`               |
| 0x05  | PLAYER_LEAVE       | C → R     | `[u128 uuid]`                                                        |
| 0x06  | CHUNK_REQUEST      | C → R     | `[u8 dim][i32 cx][i32 cz][i32 view_radius]`                          |
| 0x07  | CHUNK_DATA         | R → C     | `[u8 dim][i32 cx][i32 cz][u32 len][bytes：区块网络数据]`              |
| 0x08  | REGION_STATS       | C → R     | 空载荷；region 立即回 0x09                                           |
| 0x09  | REGION_STATS_RESP  | R → C     | `[f32 tps][f32 avg_tps_ms][u32 tick_count][u8 sleeping][u32 loaded_chunks]` |

## 生命周期

1. core 启动后连接 datarouter，注册 `icbm.core`；读取配置，为出生点区域拉起第一个 region 子进程（同一二进制 `--region` 模式）。
2. region 启动 → 连接 datarouter → 注册 `icbm.region.<id>` → 发送 `REGION_REGISTER`。
3. core 回复 `REGISTER_ACK` 后，region 进入正常模拟；每 1 秒发送一次 `REGION_HEARTBEAT`（含 TPS 与沉睡状态）。
4. 玩家进出：core 发送 `PLAYER_ENTER` / `PLAYER_LEAVE`；region 在无玩家时进入沉睡（停止 tick），持续 30 秒后由 core 发送 `REGION_UNLOAD` 卸载。
5. 区块数据：core 收到客户端进入时向所在 region 发 `CHUNK_REQUEST`，region 从 `lib/anvil` 读取或 flat 生成后返回 `CHUNK_DATA`（网络序列化），core 转发给客户端；改动经 `lib/anvil` 写回 `.mca`。

## 错误处理

- 帧长度不符 / 未知 kind：接收方丢弃并记录计数。
- 心跳超时（默认 5 秒）：core 视 region 失联，回收其租约并重启 region 进程。
- region 非正常退出：core 从注册表摘除并告警，不自动重启（避免风暴）；由显式操作或玩家需求重新拉起。
