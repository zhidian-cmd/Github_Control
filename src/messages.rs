//! 错误中文化：命令失败时，中文解释在前，原始信息附后。
//! 与 Python 版 messages.py 逐条对应。

const MAP: &[(&str, &str)] = &[
    ("Authentication failed", "认证失败，可能是未登录或 token 过期，请到设置里重新登录"),
    ("Support for password authentication was removed", "GitHub 已禁用密码认证，请使用 PAT 登录"),
    ("Repository not found", "找不到远程仓库，请检查地址和权限"),
    ("does not appear to be a git repository", "远程仓库不存在：还没关联远程，或远程仓库已删除。点 [推送] 会自动在 GitHub 创建同名私有仓库"),
    ("Could not read from remote repository", "无法读取远程仓库：请检查网络/代理，或远程仓库是否还存在"),
    ("CRYPT_E_NO_REVOCATION_CHECK", "HTTPS 证书吊销检查失败（一般是代理软件拦截导致）。程序已自动关闭吊销检查，请重试推送"),
    ("couldn't find remote ref", "远程分支不存在，可能还没推送过"),
    ("Your local changes would be overwritten", "有未提交改动，请先提交或储藏"),
    ("detected dubious ownership", "Git 检测到目录所有者异常，已通过 safe.directory 处理，重试即可"),
    ("CONFLICT", "发生冲突，请手动解决冲突文件后提交"),
    ("not a git repository", "该目录不是 Git 仓库"),
    ("failed to push some refs", "推送失败，远程有新提交，请先点 [更新] 拉取"),
    ("Network is unreachable", "网络不通，请检查代理设置"),
    ("Could not resolve host", "网络不通，无法解析域名，请检查代理设置"),
    ("could not read username", "需要认证：中转站链接只能克隆公开仓库，私有仓库请使用 GitHub 原始地址；或先登录"),
    ("Permission denied", "文件被其他程序占用或权限不足，请关闭占用它的程序（编辑器/杀毒软件）后重试"),
    ("index.lock", "Git 索引被占用：可能有另一个 git 操作在进行。请稍后重试，或手动删除仓库下 .git/index.lock 文件"),
    ("No space left", "磁盘空间不足，请清理磁盘后重试"),
    ("Nothing to commit", "没有可提交的改动"),
    ("nothing added to commit", "没有可提交的改动：文件还未被 Git 跟踪，点 [提交] 会自动暂存全部，或先点单个文件的 [暂存]"),
    ("HTTP 403", "GitHub 拒绝了请求（403），可能 token 权限不够或触发了限流"),
    ("Bad credentials", "token 无效或已过期，请重新登录"),
    ("no tracking information", "当前分支还没有关联远程分支，推送一次后会自动关联"),
    ("already exists", "已存在，换一个名字或跳过创建"),
];

/// 失败输出 → 中文提示 + 原始信息。没有命中就不动。
pub fn translate(out: &str) -> String {
    if out.is_empty() {
        return out.to_string();
    }
    let low = out.to_lowercase();
    for (key, msg) in MAP {
        if low.contains(&key.to_lowercase()) {
            let trimmed = out.trim();
            let head: String = trimmed.chars().take(800).collect();
            return format!("{msg}\n\n原始信息：\n{head}");
        }
    }
    out.to_string()
}
