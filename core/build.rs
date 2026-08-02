//! 构建脚本：注入 Git 提交哈希（`/about` 与品牌展示用）。
//!
//! 参考 master 分支 `core/build.rs`：短哈希（7 位）用于展示，完整哈希留作
//! 后续扩展；`git` 不可用或非仓库时回退为 `unknown`。

use std::process::Command;

fn main() {
    // 短哈希（7 位）用于展示
    let short = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output();
    let git_hash_short = match short {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => "unknown".to_string(),
    };

    // 完整哈希（供 /about 等展示）
    let full = Command::new("git").args(["rev-parse", "HEAD"]).output();
    let git_hash_full = match full {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => "unknown".to_string(),
    };

    println!("cargo::rerun-if-changed=../.git/HEAD");
    println!("cargo::rustc-env=GIT_HASH={git_hash_short}");
    println!("cargo::rustc-env=GIT_HASH_FULL={git_hash_full}");
}
