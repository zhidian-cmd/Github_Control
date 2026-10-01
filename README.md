# Github Control（Rust/Tauri 移植版）

本地 Git 与 GitHub 的桌面控制台 —— 原 Python（Flask + pywebview）版本的完整 Rust 移植。

> **移植原则**：Python 原工程（`D:\Source Code\python\Github_control`）一个字节都不动。
> 前端 SPA（`web/`）原样复用；后端业务层 + HTTP API 层全部用 Rust 重写；桌面窗口由 Tauri 提供（WebView2）。

## 与 Python 版的对应关系

| Python 原工程 | Rust 移植版 |
|---|---|
| `main.pyw`（pywebview 窗口 + Flask 启动） | `src/main.rs`（Tauri 窗口 + axum 本地服务） |
| `core/*.py`（业务层） | `src/*.rs`（同名模块） |
| `bridge/*.py`（Flask 路由 /api/*） | `src/api.rs`（axum 路由，接口完全一致） |
| `web/`（前端 SPA） | `web/`（原样复制，未修改） |
| `config.json` / `data/` | 同（可执行文件旁边，绿色便携） |
| `runtime/`（便携版 git.exe/gh.exe） | 同（本机测试时是指向 Python 工程 runtime 的目录联接） |

## 功能（与 Python 版一致）

- **多仓库管理**：仓库切换器下拉切换，支持打开本地仓库 / 克隆 / 新建（本地 init + 一键在 GitHub 创建远程并推送）
- **文件改动**：目录树折叠展示，每行状态徽标（M/A/D/??）+ 相对时间 + 撤销/恢复；点文件看信息卡和 diff
- **提交**：填摘要即可（自动包含全部改动），不自动推送
- **历史**：提交表格（hash / 消息 / 作者 / 时间 / 增删行数），点开看提交详情，文件行展开单文件 diff
- **回滚**：软回退 / 混合回退 / 硬回退 / 反向提交四种语义；硬回退自动打保险 tag `_backup_<时间戳>` 并二次确认
- **紧急恢复**：设置里列出保险 tag 和 reflog，一键回到任意时点
- **Release**：独立视图，多文件附件、Markdown 说明、中文名文件自动转 ASCII 临时副本
- **推送容错**：没有远程时弹窗让你起名并选择私有/公开，自动 `gh repo create` 一条命令完成创建关联推送
- **冲突解决**：进行中操作横幅（merge/rebase/cherry-pick/revert/bisect）+ 一键采用我方/对方

## 技术栈

- **Tauri 2**（窗口外壳，WebView2）
- **axum 0.8 + tokio**（本地 HTTP 服务，替代 Flask）
- **tauri-plugin-dialog**（原生文件夹/文件选择，替代 pywebview js_api + tkinter）
- **tauri-plugin-single-instance**（单实例，替代 CreateMutexW）
- 前端：原生 HTML/CSS/JS（零改动复用）

## 目录结构

```
Github_Control\            # 本移植版工程
├── Cargo.toml
├── tauri.conf.json
├── build.rs
├── capabilities\
├── icons\                 # 窗口/程序图标（复制自原工程 favicon.ico）
├── web\                   # 前端（原样复制）
├── assets\favicon.ico
├── runtime\               # 便携版 git.exe、gh.exe（本机为指向 Python 工程 runtime 的目录联接）
└── src\
    ├── main.rs            # Tauri 入口：单实例、runtime 检查、起服务、建窗口
    ├── api.rs             # axum 路由（全部 /api/*，与 Python bridge 一一对应）
    ├── paths.rs           # 路径管理（GC_BASE_DIR / GC_CONFIG_FILE 可覆盖）
    ├── config.rs          # config.json 加载/合并/保存（损坏自动备份）
    ├── env.rs             # 子进程环境（HOME 重定向、代理、schannel）
    ├── runner.rs          # git/gh 命令执行（超时、日志、错误中文化）
    ├── messages.rs        # 错误中文化（与 Python messages.py 同表）
    ├── credential.rs      # PAT 登录/登出、.gitconfig 整文件写入
    ├── git_repo.rs        # 状态/diff/提交/分支/统计
    ├── git_status_v2.rs   # porcelain=v2 全字段解析
    ├── git_rollback.rs    # 四种回退 + 保险 tag + reflog
    ├── repositories.rs    # 多仓库管理
    ├── gh_runner.rs       # Release 创建/列表/删除（代理失败直连重试）
    ├── worktree_probe.rs  # 文件系统探测（前 8KB + 末 1 字节）
    ├── asset_helper.rs    # 中文名附件转 ASCII 临时副本
    ├── net.rs             # 代理可达性检测
    └── win.rs             # 虚拟屏幕钳窗、原生消息框
```

## 构建与运行

```bash
cargo build --release        # 产物：target\release\github-control.exe
# 或直接
cargo run
```

绿色便携：把 `target\release\github-control.exe` 拷到工程根（或任意目录，与
`runtime/`、`web/` 同级）双击即可运行；exe 位于 `target\release|debug` 下时
程序会自动把根目录上溯到工程根。

运行前提：

1. `runtime/` 里有便携版 `git.exe`（`runtime\git\cmd\git.exe`）和 `gh.exe`。
   本机测试时 `runtime` 是指向 `D:\Source Code\python\Github_control\runtime` 的目录联接；
   换机器后把 Python 工程里的 runtime 整个复制过来即可，或删除联接放入自己的便携版。
2. Windows 10/11 + WebView2 运行时（系统一般自带）。

### 环境变量（可选）

| 变量 | 作用 |
|---|---|
| `GC_BASE_DIR` | 指定程序根目录（config.json / data / runtime / web 的查找根），默认 = exe 所在目录 |
| `GC_CONFIG_FILE` | 指定 config.json 路径（测试隔离用） |

## 端口与安全

- HTTP 服务只绑定 `127.0.0.1` 随机端口，局域网内其他设备无法访问
- 窗口加载本地服务 URL；前端仍通过同源 `fetch('/api/*')` 通信
- 原生对话框通过注入到 index.html 的桥接脚本（`window.pywebview.api`）转到本地 API，前端源码零修改

## 日志与排查

| 日志 | 内容 |
|---|---|
| `data/logs/run.log` | 每条 git/gh 命令、返回码、输出前 2000 字符（超 1MB 轮转 run.old.log） |
| `data/logs/error.log` | 后端异常（带 trace_id）+ 前端未捕获异常 |
| `data/logs/clone_<id>.log` | 克隆进度 |

界面报错都是中文 + 原始信息附后。遇到「点了没反应」，先翻 `data/logs/run.log`。

## 性能优化（2026-09 基准，测试仓库=Python 原工程，干净工作区）

| 接口 | 优化前（中位） | 优化后（中位） | 说明 |
|---|---|---|---|
| `/api/repo/status_hardcore` | 234 ms | **冷 58 ms / 热 56 ms** | 4 个独立 git 子进程并行（`tokio::join!`），文件探测/行数统计移出 async |
| `/api/git/log_table`（n=50） | 238 ms | **冷 150 ms / 热 40 ms** | 按 (repo, HEAD) 缓存 numstat 历史，HEAD 未变只跑毫秒级 `rev-parse` |
| `/api/repo/overview` | 219 ms | 约 20 ms 区间 | — |
| `/api/repo/status` | 148 ms | 约 60 ms 区间 | — |

其余优化：未跟踪文件行数统计只读前 2MB（大文件不再全量读入，超限标记
`plus_truncated`）；binary 文件跳过统计；`get_remote` 正则 `OnceLock` 只编译一次；
`[profile.release]` 加 `lto="thin" codegen-units=1 strip=true`（exe 13.5MB → 11.9MB，
启动更快）。基准方法：启动后立即连打请求（进程可能因用户关窗而退出，属正常）。

`GC_HEADLESS=1` 环境变量：只启动本地服务、不创建窗口，供无打扰压测（正常使用不设置）。

## 已处理的坑（与原版一致）

- Windows `safe.directory=*`、`core.quotepath=false`、`logOutputEncoding=utf-8` 登录时自动写入
- MITM 代理下 schannel 吊销检查必失败：自动 `http.sslBackend=schannel` + `schannelCheckRevoke=false`
- 凭据 helper 段落顺序：`.gitconfig` 由程序整文件写入，启动时自动修正旧格式
- 窗口最小化时坐标不再保存，启动时钳回虚拟桌面内
- config.json 损坏自动备份并按默认配置启动
- 硬回退 / 删除分支 / 放弃改动等危险操作全部二次确认，代码层强制 `confirm=true`

## 移植期踩过的坑（Rust 特有）

- **axum 0.8 通配符路由**：`Router.route("/{*path}", ...)` + `Path<String>` 在本机
  实测捕获恒为空串（静态文件全 404、界面白屏）。改为把所有未匹配路径走
  `router.fallback(handler)`，路径直接从 `req.uri().path()` 取，绕开通配符捕获语义。
- **tauri 2 `WebviewWindowBuilder` 加载本地 URL**：用 `WebviewUrl::External(...)`，
  不能用 `&str`。
- **tauri-plugin-dialog 阻塞对话框**：`blocking_pick_folder()` 返回
  `Option<FilePath>`，转字符串用 `p.as_path()`（闭包内转 owned，避免生命周期错误）。
- **子进程超时杀进程**：`wait_with_output()` 会消费 `child`，超时分支无法再
  `kill()`。改为先 `child.stdout.take()`/`stderr.take()`，再 `child.wait()`，
  超时时 kill 仍可用。
- **`std::io::Error` 不实现 `Clone`**：`Result<PathBuf, io::Error>` 不能 `.clone()`，
  用 `as_ref()` 匹配。
- **cargo 走系统代理失败**：本机代理端口不通导致 crates.io 下载失败；在工程级
  `.cargo/config.toml` 写 `[http] proxy = ""` 直连（只影响本工程）。
- **tauri 2 不弹新窗口**：WebView 里 `target="_blank"` 链接（登录页"点此直达创建页"、
  Release 详情"查看"）默认被吞、点击无效。修复：首页注入的 SHIM 脚本拦截
  `a[target="_blank"]` 点击，转 POST `/api/gh/open_url`（仅允许 http/https，用
  ShellExecuteW 调系统默认浏览器），前端文件零改动。
- **移动工程目录后 build 报旧路径**：tauri-build 缓存残留旧绝对路径导致
  `failed to read file '\\?\D:\旧位置\...'`；`cargo clean` 后全量重建即可。
