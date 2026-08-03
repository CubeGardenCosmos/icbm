"""ICBM 开发环境工具（Python 侧）。

分层约定（单向依赖，禁止反向/横向引用）：
    main.py -> icbm_dev_env.commands
            -> {decompile, vcs, toolchains}
            -> {net, archives, util, config}

当前仅依赖标准库；第三方依赖由 uv 管理（见 build/pyproject.toml）。
"""

__version__ = "0.1.0"
