//! 错误中文化：命令失败时，中文解释在前，原始信息附后。
//! 与 Python 版 messages.py 逐条对应。

const MAP: &[(&str, &str)] = &[
    // —— 网络 / 代理（本轮实机踩过的坑全部收录）——
    ("schannel", "HTTPS 连接异常中断（多为代理节点不稳或网络切换导致）。已自动重试仍失败，请稍后再试，或更换代理软件里的 GitHub 节点"),
    ("early eof", "连接被意外中断（网络/代理不稳定），请重试"),
    ("failed to connect", "无法连接到服务器：请检查网络，或到设置页检查代理配置"),
    ("connect tunnel failed", "代理隧道建立失败（节点拒绝或上游异常），请更换代理节点，或在设置里关闭代理直连重试"),
    ("proxy connect aborted", "代理连接被中断，请更换代理节点或关闭代理重试"),
    ("recv failure", "接收数据失败（网络中断），请重试或检查代理"),
    ("ssl connect error", "SSL 连接失败，请检查代理或网络"),
    ("rpc failed", "网络传输中断（RPC failed）：多为代理不稳或大文件传输失败。已自动重试，仍失败请更换节点再试"),
    ("stale info", "远程分支状态与本地记录不一致（远程刚有变动）。程序会自动获取最新状态并重试一次；若仍失败，请再点一次推送"),
    ("need to specify how to reconcile", "本地与远程分支历史分叉，Git 需要指定合并方式。请重试一次，程序会自动以合并方式处理"),
    // —— 认证 ——
    ("to get started with github cli", "gh 命令行未登录 GitHub，请到设置页登录后重试"),
    ("token in keyring is invalid", "gh 判定本地保存的 token 失效。若你近期登录过且没改过 token，多半是代理节点瞬时异常——稍等重试一次；反复出现请到设置页重新登录"),
    ("tell me who you are", "还没有设置 Git 身份（用户名/邮箱），无法提交。到设置页登录 GitHub 后会自动配置，重试即可"),
    ("returned error: 403", "GitHub 拒绝了请求（403）：token 权限不够、触发限流，或分支受保护"),
    ("returned error: 404", "远程资源不存在（404）：仓库可能已删除，或私有仓库未登录无权访问"),
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
    ("failed to push some refs", "推送被拒绝：远程分支有你本地没有的新提交，请先点 [更新] 拉取合并"),
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
    ("not a valid", "名称不合法：请检查分支/标签名的格式（不能含空格和 ~^:?*[\\ 字符）"),
    ("invalid reference", "引用不存在：分支/标签名可能写错了"),
    ("does not exist", "远程仓库不存在或地址写错，请核对 URL 后重试"),
    ("already exists", "已存在，换一个名字或跳过创建"),
    // —— 兜底：bare "not found" 放最后，避免抢先命中更具体的条目 ——
    ("not found", "未找到：仓库/文件可能已删除，或私有仓库未登录无权访问"),
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

/// 命中映射表时返回 Some(中文提示)，否则 None。
/// 给克隆日志这类"原始输出已在场、只需要补一句人话"的场景用。
pub fn matched_hint(out: &str) -> Option<String> {
    if out.trim().is_empty() {
        return None;
    }
    let low = out.to_lowercase();
    for (key, msg) in MAP {
        if low.contains(&key.to_lowercase()) {
            return Some(msg.to_string());
        }
    }
    None
}
