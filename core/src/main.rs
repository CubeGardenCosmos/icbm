//! icbm 服务器主二进制（core 模式）。
//!
//! 与 region 模式共用同一二进制：携带 `--region` 参数时路由到
//! `icbm_region`，否则以 core 模式启动。此处的入口在后续阶段接入
//! 配置加载、datarouter 初始化与 region 编排。
#[allow(clippy::missing_const_for_fn)] // 入口函数不能为 const
fn main() {}
