/* Github Control v2.1 — SPA 前端
   布局：顶栏（仓库切换/更新/推送/发布/登录/设置）+ 侧边栏（改动/历史+提交框）+ 主内容区
   约定：所有请求走 api()；未登录时联网按钮灰掉；危险操作必须走 confirmDialog */

"use strict";

/* ---------- 基础工具 ---------- */

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => [...document.querySelectorAll(sel)];

function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

async function api(path, { method = "GET", body } = {}) {
  let res;
  try {
    res = await fetch(path, {
      method,
      headers: body ? { "Content-Type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch {
    throw new Error("无法连接后端服务，请重启程序");
  }
  const data = await res.json().catch(() => ({ ok: false, error: `响应解析失败 (${res.status})` }));
  if (!res.ok || data.ok === false) {
    const fallback = {
      400: "请求参数有误",
      401: "登录状态已失效，请到设置页重新登录",
      403: "没有权限执行该操作",
      404: "接口不存在（可能程序版本不一致，请重启）",
      500: "后端内部错误，请重启程序；若反复出现请看日志",
      502: "后端服务不可达，请重启程序",
      503: "后端服务不可达，请重启程序",
    }[res.status] || `请求失败 (${res.status})`;
    const msg = data.error || fallback;
    // Token 中途过期：顶栏徽章立刻变黄（鲁棒 #6）
    if (/Bad credentials|HTTP 401|token 无效|登录已过期/.test(msg)) {
      STATE.auth = { state: "expired", username: "" };
      renderAuthChip();
      applyAuthLocks();
    }
    throw new Error(msg);
  }
  return data;
}

function syncRemoteFromStatus(st) {
  // 前端对比 remote 缓存，变了才发一次 POST（不再后端每次 status 写盘）
  if (!st?.remote || !repo()) return;
  const rec = STATE.repos.find((r) => r.path === repo());
  if (rec && rec.remote !== st.remote) {
    rec.remote = st.remote;
    api("/api/repo/sync_remote", { method: "POST", body: { path: repo(), remote: st.remote } })
      .catch(() => {});
  }
}

/* 全局错误兜底：任何未捕获异常 → toast + 写入后端 error.log（稳定 #1） */
window.addEventListener("error", (e) => reportFrontendError(e.message, e.error?.stack || ""));
window.addEventListener("unhandledrejection", (e) => {
  reportFrontendError(String(e.reason?.message || e.reason), e.reason?.stack || "");
});
function reportFrontendError(msg, stack) {
  toast(`页面异常：${msg}`, "error");
  fetch("/api/client_log", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ msg, stack: (stack || "").slice(0, 2000) }),
  }).catch(() => {});
}

/* 后端健康检查：连续 3 次失败提示重启（稳定 #3） */
let healthFails = 0;
setInterval(async () => {
  try {
    await fetch("/api/health", { method: "GET" });
    healthFails = 0;
  } catch {
    if (++healthFails === 3) {
      toast("后端服务已断开，请重启程序", "error");
    }
  }
}, 5000);

/* 顶栏不确定进度条：长操作期间显示（流畅 #1） */
let progressUsers = 0;
function progressOn() {
  if (++progressUsers === 1) $("#progress")?.classList.add("on");
}
function progressOff() {
  if (progressUsers > 0 && --progressUsers === 0) $("#progress")?.classList.remove("on");
}

function toast(msg, type = "", action = null) {
  const box = $("#toasts");
  if (!box) { console.error("[toast]", msg); return; }  // DOM 未就绪时只打控制台，绝不递归崩页
  while (box.children.length >= 3) box.firstChild.remove();  // 最多同屏 3 条（流畅 #7）
  const el = document.createElement("div");
  el.className = `toast ${type}`;
  if (action) {
    const wrap = document.createElement("span");
    wrap.style.display = "flex";
    wrap.style.alignItems = "center";
    wrap.style.gap = "10px";
    const span = document.createElement("span");
    span.textContent = msg;
    const btn = document.createElement("button");
    btn.className = "btn btn-sm";
    btn.textContent = action.label;
    btn.onclick = () => { el.remove(); action.onclick?.(); };
    wrap.append(span, btn);
    el.appendChild(wrap);
  } else {
    el.textContent = msg;
  }
  box.appendChild(el);
  setTimeout(() => el.remove(), action ? 6000 : (type === "error" ? 6000 : 3000));
}

async function runBtn(btn, fn) {
  if (!btn || btn.disabled) return;
  const old = btn.innerHTML;
  btn.disabled = true;
  btn.textContent = "处理中…";
  progressOn();
  try {
    await fn();
  } catch (e) {
    toast(e.message, "error");
  } finally {
    progressOff();
    btn.disabled = false;
    btn.innerHTML = old;
    applyAuthLocks();
  }
}

function relTime(v) {
  if (!v) return "";
  const d = typeof v === "number" ? new Date(v * 1000) : new Date(v);
  const diff = (Date.now() - d.getTime()) / 1000;
  if (isNaN(diff)) return "";
  if (diff < 60) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  if (diff < 86400 * 7) return `${Math.floor(diff / 86400)} 天前`;
  const p = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/* 硬核视图：XY 双色块（左=HEAD→索引，右=索引→工作区） */
function xyPair(x, y) {
  const cell = (ch) => {
    if (ch === "." || ch === " ") return `<span class="xy xy-none" title="无变化">·</span>`;
    const cls = { M: "xy-mod", A: "xy-add", D: "xy-del",
                  R: "xy-ren", C: "xy-cp", U: "xy-conf",
                  "?": "xy-unt", "!": "xy-ign" }[ch] || "xy-mod";
    const tip = { M: "修改", A: "新增", D: "删除", R: "重命名", C: "复制",
                  U: "冲突", "?": "未跟踪", "!": "已忽略" }[ch] || ch;
    return `<span class="xy ${cls}" title="${tip}">${esc(ch)}</span>`;
  };
  return `<span class="xy-pair">${cell(x)}${cell(y)}</span>`;
}

/* ---------- 原生对话框（pywebview） ---------- */

const native = () => window.pywebview?.api ?? null;

async function pickFolder(input) {
  if (native()?.pick_folder) {
    const p = await native().pick_folder();
    if (p) input.value = p;
  } else {
    const p = prompt("输入文件夹路径：", input.value);
    if (p) input.value = p;
  }
}

async function pickFiles(cb) {
  if (native()?.pick_files) {
    const ps = await native().pick_files();
    if (ps?.length) cb(ps);
  } else {
    const p = prompt("输入文件完整路径（多个用 | 分隔）：", "");
    if (p) cb(p.split("|").map((s) => s.trim()).filter(Boolean));
  }
}

/* ---------- 对话框与菜单 ---------- */

function openDialog(id) { $(id).classList.add("open"); }
function closeDialog(id) {
  $(id).classList.remove("open");
  if (id === "#generic-overlay") {
    // 清掉遗留的确定按钮 handler，防止点空白退出后误触发
    const ok = $("#generic-ok");
    if (ok) ok.onclick = null;
    // 分支下拉若停在"＋ 新建分支…"上，复位到当前分支
    const sel = $("#branch-select");
    if (sel && sel.value === "__new__" && STATE.status?.branch) {
      sel.value = STATE.status.branch;
    }
  }
}
document.addEventListener("click", (e) => {
  if (e.target.classList?.contains("overlay")) closeDialog("#" + e.target.id);
  if (!e.target.closest(".dropdown") && !e.target.closest(".repo-switch")) hideMenu();
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { $$(".overlay.open").forEach((o) => closeDialog("#" + o.id)); hideMenu(); }
});

/** 通用确认框：{title, html, confirmText, danger} → Promise<bool> */
function confirmDialog({ title, html = "", confirmText = "确认", danger = false }) {
  return new Promise((resolve) => {
    $("#confirm-title").textContent = title;
    $("#confirm-body").innerHTML = html;
    const ok = $("#confirm-ok");
    ok.textContent = confirmText;
    ok.className = `btn ${danger ? "btn-danger" : "btn-primary"}`;
    openDialog("#confirm-overlay");
    const done = (v) => {
      closeDialog("#confirm-overlay");
      ok.onclick = null;
      $("#confirm-cancel").onclick = null;
      resolve(v);
    };
    ok.onclick = () => done(true);
    $("#confirm-cancel").onclick = () => done(false);
  });
}

/** 浮动菜单：items = [{label, sub, danger, checked, onclick, oncontextmenu}]
 *  opts.compact = 紧凑条目；opts.scrollCount = 前 N 条放进滚动区（最多显示 4 条），
 *  其余条目（如操作按钮）固定在滚动区外不随滚动 */
function showMenu(items, x, y, opts = {}) {
  const el = $("#menu");
  el.innerHTML = "";
  el.classList.toggle("compact", !!opts.compact);
  let scrollBox = null;
  if (opts.compact && opts.scrollCount > 0) {
    scrollBox = document.createElement("div");
    scrollBox.className = "menu-scroll";
    el.appendChild(scrollBox);
  }
  let inScroll = 0;
  for (const it of items) {
    if (it.divider) { el.insertAdjacentHTML("beforeend", `<div class="menu-divider"></div>`); continue; }
    const useScroll = scrollBox && inScroll < opts.scrollCount;
    if (useScroll) inScroll++;
    const b = document.createElement("button");
    b.className = `menu-item ${it.danger ? "danger" : ""} ${it.checked ? "active" : ""}`;
    b.innerHTML = `${esc(it.label)}${it.sub ? `<span class="sub">${esc(it.sub)}</span>` : ""}`;
    b._it = it;  // 事件直接挂 item 引用，不用下标回配（divider 会错位）
    b.onclick = () => { hideMenu(); it.onclick?.(); };
    if (it.oncontextmenu) {
      b.oncontextmenu = (ev) => { ev.preventDefault(); hideMenu(); it.oncontextmenu(ev); };
    }
    (useScroll ? scrollBox : el).appendChild(b);
  }
  el.classList.add("open");
  const r = el.getBoundingClientRect();
  el.style.left = Math.min(x, innerWidth - r.width - 8) + "px";
  el.style.top = Math.min(y, innerHeight - r.height - 8) + "px";
}
function hideMenu() { $("#menu")?.classList.remove("open"); }

/* ---------- 全局状态 ---------- */

const STATE = {
  auth: { state: "loading", username: "" },
  repos: [],
  active: "",          // 当前仓库路径
  status: null,        // active 仓库的 status
  commits: [],
  tab: "changes",      // changes | history
  selFile: null,
  selCommit: null,
  filter: "",
  expandedDirs: new Set(),  // 目录树折叠状态
  uiExpanded: {},           // 每仓库的展开目录表（持久化到 config.json）
  lastCloneDir: "",         // 上次克隆的保存位置，下次打开克隆窗口自动填
};

const repo = () => STATE.active;

function applyAuthLocks() {
  const bad = STATE.auth.state !== "logged_in";
  $$("[data-needs-auth]").forEach((b) => {
    b.disabled = bad || (b.dataset.needsRepo === "1" && !repo());
    b.title = bad ? (STATE.auth.state === "expired" ? "登录已过期，请重新登录" : "请先登录") : "";
  });
}

/* ---------- 顶栏 ---------- */

function renderTopbar() {
  const cur = STATE.repos.find((r) => r.path === repo());
  $("#topbar").innerHTML = `
    <button class="repo-switch" id="repo-switch">
      <span>📁</span>
      <span class="name">${esc(cur ? cur.name : repo() ? "仓库" : "选择仓库")}</span>
      <span class="caret">▼</span>
    </button>
    <button class="btn" id="op-pull" data-needs-auth data-needs-repo="1">🔄 更新</button>
    <button class="btn" id="op-push" data-needs-auth data-needs-repo="1">⬆ 推送</button>
    <button class="btn" id="op-release" data-needs-auth data-needs-repo="1">📦 发布</button>
    <button class="icon-btn" id="op-explorer" data-needs-repo="1" title="在资源管理器中打开仓库">📂</button>
    <button class="icon-btn" id="op-web" data-needs-auth data-needs-repo="1" title="在浏览器打开 GitHub 页面">🌐</button>
    <span class="spacer"></span>
    <button class="auth-chip" id="auth-chip"><span class="dot"></span>检查中…</button>
    <button class="icon-btn" id="btn-settings" title="设置">⚙</button>`;

  renderAuthChip();
  applyAuthLocks();

  $("#repo-switch").onclick = (e) => {
    e.stopPropagation();
    openRepoMenu(e.currentTarget);
  };
  $("#op-pull").onclick = (ev) => runBtn(ev.target, async () => {
    const d = await api("/api/git/pull", { method: "POST", body: { repo: repo() } });
    if (!d.ok) throw new Error(d.error || "更新失败");
    await refreshAll();
    toast("已更新", "success");
  });
  $("#op-push").onclick = (ev) => runBtn(ev.target, async () => {
    const baseName = repo().replace(/[\\/]+$/, "").split(/[\\/]/).pop();
    // 没有远程仓库时，先让用户选名字和可见性
    const ho = await api(`/api/git/has_origin?path=${encodeURIComponent(repo())}`);
    let create = null;
    if (!ho.has_origin) {
      create = await askCreateRemote(baseName);
      if (!create) return;
    }
    let d;
    try {
      d = await api("/api/git/push", { method: "POST", body: { repo: repo(), create } });
    } catch (e) {
      // 远程地址存在但仓库已不存在（被删/没建成功）→ 主动询问是否重建
      if (!/Repository not found|找不到远程仓库/.test(e.message)) throw e;
      const ok = await confirmDialog({
        title: "远程仓库不存在",
        html: "远程地址指向的仓库已被删除或从未创建成功。是否创建一个新的远程仓库并推送？",
        confirmText: "创建",
      });
      if (!ok) throw e;
      create = await askCreateRemote(baseName);
      if (!create) throw e;
      d = await api("/api/git/push", { method: "POST", body: { repo: repo(), create } });
    }
    if (!d.ok) throw new Error(d.error || "推送失败");
    await refreshAll();
    toast(d.hint || "推送完成", "success");
  });
  $("#op-release").onclick = () => showView("#release");
  $("#op-explorer").onclick = (ev) => runBtn(ev.target, async () => {
    const d = await api("/api/repo/open_explorer", { method: "POST", body: { path: repo() } });
    if (!d.ok) throw new Error(d.error || "打开失败");
  });
  $("#op-web").onclick = (ev) => runBtn(ev.target, async () => {
    const d = await api("/api/gh/open_web", { method: "POST", body: { path: repo() } });
    if (!d.ok) throw new Error(d.error || "打开失败");
    toast("已在浏览器打开 GitHub 页面", "success");
  });
  $("#btn-settings").onclick = () => showView("#settings");
}

/** 目录名/任意字符串 → GitHub 合法仓库名（非法字符替换为 -，首尾 -._ 去掉） */
function safeRepoName(s) {
  return String(s || "")
    .replace(/[^A-Za-z0-9._-]+/g, "-")
    .replace(/^[-._]+|[-._]+$/g, "");
}

/** 推送时创建远程仓库的弹窗 → Promise<{name, private} | null> */
function askCreateRemote(defaultName) {
  return new Promise((resolve) => {
    // 中文/特殊字符目录名也净化成合法名预填，而不是留空让用户从零起名
    const safe = safeRepoName(defaultName);
    $("#generic-title").textContent = "创建远程仓库";
    $("#generic-body").innerHTML = `
      <p style="margin-top:0">本地仓库还没有远程地址。将在你的 GitHub 账号下创建仓库并推送：</p>
      <div class="field"><label>仓库名（只能用英文、数字、-、_、.）</label>
        <input class="input" id="cr-name" value="${esc(safe)}" placeholder="如 netease-player"></div>
      <label class="check" style="margin-bottom:8px">
        <input type="radio" name="cr-vis" value="private" checked> 私有 Private（只有你能看到）
      </label>
      <label class="check">
        <input type="radio" name="cr-vis" value="public"> 公开 Public（所有人可见）
      </label>
      <div id="cr-probe" class="hint" style="min-height:18px"></div>`;
    $("#generic-ok").textContent = "创建并推送";
    // 先探测账号下有没有同名仓库：有则提示改为"关联并推送"，避免创建撞车
    if (safe) {
      api(`/api/gh/find_own_repo?name=${encodeURIComponent(safe)}`)
        .then((d) => {
          const box = $("#cr-probe");
          if (!box || !d.ok || !d.exists) return;
          box.innerHTML = `检测到你的 GitHub 上已有 <b>${esc(d.login)}/${esc(d.name)}</b>，确认后将直接关联并推送（不会重建，以本地内容为准）。`;
          $("#generic-ok").textContent = "关联并推送";
        })
        .catch(() => {});
    }
    openDialog("#generic-overlay");
    $("#generic-ok").onclick = () => {
      const name = $("#cr-name").value.trim();
      if (!/^[A-Za-z0-9._-]+$/.test(name)) {
        toast("仓库名只能用英文、数字、-、_、.（GitHub 不接受中文名）", "error");
        return;
      }
      const vis = document.querySelector("input[name=cr-vis]:checked").value;
      closeDialog("#generic-overlay");
      $("#generic-ok").onclick = null;
      resolve({ name, private: vis === "private" });
    };
  });
}

function renderAuthChip() {
  const chip = $("#auth-chip");
  chip.className = `auth-chip state-${STATE.auth.state}`;
  const label = {
    logged_in: `已登录 ${STATE.auth.username || ""}`,
    expired: "已过期 · 重新登录",
    not_logged_in: "未登录 · 去登录",
    loading: "检查中…",
  }[STATE.auth.state] || "未登录";
  chip.innerHTML = `<span class="dot"></span>${esc(label)}`;
  chip.onclick = () => showView(STATE.auth.state === "logged_in" ? "#settings" : "#login");
}

function openRepoMenu(anchor) {
  const r = anchor.getBoundingClientRect();
  const items = STATE.repos.map((rp) => ({
    label: rp.name,
    sub: rp.path,
    checked: rp.path === repo(),
    onclick: () => afterRepoReady(rp.path),
    oncontextmenu: (ev) => {
      ev.preventDefault();
      hideMenu();
      showMenu([
        { label: "重命名", onclick: () => {
            const name = prompt("新名称：", rp.name);
            if (name?.trim()) api("/api/repo/rename", { method: "POST", body: { path: rp.path, name } })
              .then(loadRepos).catch((e) => toast(e.message, "error"));
          } },
        { label: "从列表移除", danger: true, onclick: async () => {
            const ok = await confirmDialog({
              title: `移除 ${rp.name}？`,
              html: "只从列表移除，<b>不会删除文件夹</b>。",
              confirmText: "移除", danger: true,
            });
            if (!ok) return;
            await api("/api/repo/remove", { method: "POST", body: { path: rp.path } });
            if (rp.path === repo()) { STATE.active = ""; STATE.status = null; }
            await loadRepos();
            renderAll();
          } },
      ], ev.clientX, ev.clientY);
    },
  }));
  items.push({ divider: true });
  items.push({ label: "＋ 打开本地仓库", onclick: openLocalRepo });
  items.push({ label: "＋ 克隆仓库", onclick: openCloneDialog });
  items.push({ label: "＋ 新建仓库", onclick: openNewRepoWizard });
  showMenu(items, r.left, r.bottom + 6, { compact: true, scrollCount: STATE.repos.length });
}

/* ---------- 侧边栏 ---------- */

function renderSidebar() {
  const hasRepo = !!repo();
  $("#sidebar").style.display = hasRepo ? "" : "none";
  if (!hasRepo) return;
  const keepListScroll = ($(".side-list") || {}).scrollTop || 0;  // 整栏重建时保住滚动位置

  const s = STATE.status;
  const hc = STATE.hc;
  const files = (hc?.files || []).filter((f) =>
    !STATE.filter || f.path.toLowerCase().includes(STATE.filter.toLowerCase()));

  if (STATE.tab === "changes") {
    const ipLabel = { merge: "merge", rebase: "rebase", "cherry-pick": "cherry-pick", revert: "revert", bisect: "bisect" }[hc?.in_progress] || "";
    const conflicts = (hc?.files || []).filter((f) => "conflict" in f);
    const CONFLICT_TEXT = { UU: "双方修改", AA: "双方新增", DU: "我方删除 · 对方修改", UD: "我方修改 · 对方删除", AU: "我方新增", UA: "对方新增" };
    $("#sidebar").innerHTML = `
      <div class="tabs">
        <button class="active" data-tab="changes">文件改动</button>
        <button data-tab="history">历史</button>
      </div>
      ${ipLabel ? `<div class="banner banner-danger ip-banner">
        <span>⚠ 正在 <b>${esc(ipLabel)}</b> 中，请先处理</span>
        <div class="ip-btns">
          <button class="btn btn-sm" id="ip-abort">中止</button>
          ${ipLabel !== "bisect" ? `<button class="btn btn-sm btn-primary" id="ip-continue">继续</button>` : ""}
        </div>
      </div>` : ""}
      ${conflicts.length ? `<div class="conflict-panel">
        <div class="cp-head">解决冲突（${conflicts.length}）</div>
        ${conflicts.map((f) => `
          <div class="cp-row" data-file="${esc(f.path)}">
            <div class="cp-file" title="${esc(f.path)}">
              <span class="st-U">${esc(f.conflict)}</span>
              <span class="p">${esc(f.path.split("/").pop())}</span>
              <span class="cp-type" title="${esc(CONFLICT_TEXT[f.conflict] || f.conflict)}">${esc(CONFLICT_TEXT[f.conflict] || "")}</span>
            </div>
            <div class="cp-btns">
              <button class="btn btn-sm" data-cfile="${esc(f.path)}" data-cside="ours">用我方</button>
              <button class="btn btn-sm" data-cfile="${esc(f.path)}" data-cside="theirs">用对方</button>
            </div>
          </div>`).join("")}
      </div>` : ""}
      <div class="side-head">
        <select class="input filter-input" id="branch-select" style="flex:1" title="切换分支"></select>
        <button class="icon-btn" id="branch-del" title="删除下拉框中选中的分支">🗑</button>
      </div>
      <div class="side-head">
        <input class="input filter-input" id="file-filter" placeholder="筛选文件…" value="${esc(STATE.filter)}">
      </div>
      ${hc ? (() => {
        // 极简汇总：警告 / 增删 / 储藏
        const warnFiles = hc.files.filter((f) => fileWarnings(f).length).length;
        const hasPm = hc.summary.plus || hc.summary.minus;
        const stash = hc.meta.stash || 0;
        if (!warnFiles && !hasPm && !stash) return "";
        return `<div class="side-head hc-sum">
          ${warnFiles ? `<span class="hwarn yellow" title="${warnFiles} 个文件有警告">⚠ ${warnFiles}</span>` : ""}
          ${hasPm ? `<span><span class="ns-plus">+${hc.summary.plus}</span> <span class="ns-minus">-${hc.summary.minus}</span></span>` : ""}
          ${stash ? `<span title="git stash 数量">储藏 ${stash}</span>` : ""}
        </div>`;
      })() : ""}
      <div class="side-list" id="file-list"></div>
      <div class="commit-box">
        ${s?.files.length ? `
          <input class="input" id="commit-summary" placeholder="摘要（必填）">
          <textarea class="input" id="commit-desc" placeholder="描述（可选）"></textarea>
          <button class="btn btn-primary commit-btn" id="btn-commit">提交到 ${esc(s?.branch || "")}</button>
        ` : `<div class="hint">没有可提交的改动</div>`}
      </div>`;
    renderFileList(files);
    bindChangesOps();
  } else {
    $("#sidebar").innerHTML = `
      <div class="tabs">
        <button data-tab="changes">文件改动</button>
        <button class="active" data-tab="history">历史</button>
      </div>
      <div class="side-head" style="font-size:12px;color:var(--text-2)">
        ${esc(s?.branch || "")} · 最近 50 条
      </div>
      <div class="side-list" id="commit-list"></div>`;
    renderCommitList();
  }

  const newList = $(".side-list");
  if (newList && keepListScroll) newList.scrollTop = keepListScroll;

  $$(".tabs button").forEach((b) => b.onclick = () => {
    STATE.tab = b.dataset.tab;
    STATE.selFile = null;
    STATE.selCommit = null;
    showView(STATE.tab === "changes" ? "#changes" : "#history");
    renderSidebar();
  });
}

/* 侧边栏目录树：扁平 files → 树，支持折叠（sessionStorage） */

function buildTree(files) {
  const root = { dirs: new Map(), files: [] };
  for (const f of files) {
    const parts = f.path.split("/");
    let node = root;
    for (let i = 0; i < parts.length - 1; i++) {
      if (!node.dirs.has(parts[i])) node.dirs.set(parts[i], { dirs: new Map(), files: [] });
      node = node.dirs.get(parts[i]);
    }
    node.files.push(f);
  }
  return root;
}

function countTreeFiles(node) {
  let n = node.files.length;
  for (const d of node.dirs.values()) n += countTreeFiles(d);
  return n;
}

function collectDirPaths(node, prefix, out) {
  for (const [name, child] of node.dirs) {
    const full = prefix ? `${prefix}/${name}` : name;
    out.push(full);
    collectDirPaths(child, full, out);
  }
  return out;
}

let _dirsRepo = null;

function loadExpandedDirs() {
  if (_dirsRepo === repo()) return;  // 同一仓库只加载一次，内存是唯一事实来源
  _dirsRepo = repo();
  STATE.expandedDirs = new Set(STATE.uiExpanded[repo()] || []);
}

let _uiSaveTimer = null;
function persistExpandedDirs() {
  STATE.uiExpanded[repo()] = [...STATE.expandedDirs];
  clearTimeout(_uiSaveTimer);
  _uiSaveTimer = setTimeout(() => {
    api("/api/config", { method: "POST", body: { ui: { expanded_dirs: STATE.uiExpanded } } })
      .catch(() => {});
  }, 300);  // 短 debounce：防连点写爆，窗口很小所以刷新也几乎不会丢
}

/* ---------- 硬核视图：分组 / 警告 / 三种密度（第二阶段） ---------- */

const _groupCollapsed = new Set();  // 组折叠（会话级）

function groupHcFiles(files) {
  // 未暂存是最常见的状态：不给组头，直接平铺；其余三种才加头
  return [
    ["conflicts", "冲突", files.filter((f) => "conflict" in f)],
    ["staged", "已暂存", files.filter((f) => f.x !== "." && f.x !== "?" && f.x !== "!" && !("conflict" in f))],
    ["", "", files.filter((f) => f.y !== "." && f.x === "." && !("conflict" in f))],
    ["untracked", "未跟踪", files.filter((f) => f.x === "?")],
  ].filter(([, , list]) => list.length > 0);
}

function fileWarnings(f) {
  const w = [];
  const fs = f.fs || {};
  if (fs.suspicious_secret) w.push(["red", "疑似机密文件"]);
  if (fs.size > 10 * 1024 * 1024) w.push(["red", `超大文件 ${fmtSize(fs.size)}`]);
  else if (fs.size > 1024 * 1024) w.push(["red", `大文件 ${fmtSize(fs.size)}`]);
  if (fs.is_binary) w.push(["blue", "二进制"]);
  if (fs.crlf_mixed) w.push(["yellow", "行尾混用"]);
  if (fs.pure_crlf) w.push(["gray", "纯 CRLF"]);
  if (fs.exists && !fs.is_binary && !fs.final_newline) w.push(["yellow", "缺少最后换行"]);
  if (fs.mode === "0o755") w.push(["blue", "可执行位"]);
  if (f.rename_score !== null && f.rename_score !== undefined && f.rename_score < 60)
    w.push(["yellow", `重命名相似度 ${f.rename_score}%`]);
  return w;
}

function warnIcon(f) {
  const w = fileWarnings(f);
  if (!w.length) return "";
  const worst = w.some((x) => x[0] === "red") ? "red" : w.some((x) => x[0] === "yellow") ? "yellow" : "blue";
  return `<span class="hwarn ${worst}" title="${esc(w.map((x) => x[1]).join("、"))}">⚠</span>`;
}


function fileRowHtml(f, depth) {
  const deleted = f.x === "D" || f.y === "D";
  const fs = f.fs || {};
  const pm = (f.plus || f.minus)
    ? `<span class="pm"><span class="ns-plus">+${f.plus || 0}</span> <span class="ns-minus">-${f.minus || 0}</span></span>` : "";
  const ops = f.x === "?"
    ? `<span class="help" style="font-size:11px">${fs.suspicious_secret ? "⚠机密?" : "新文件"}</span>`
    : `<button class="btn btn-sm" data-undo="${esc(f.path)}" data-deleted="${deleted ? 1 : ""}">${deleted ? "恢复" : "撤销"}</button>`;
  const pad = `padding-left:${10 + depth * 16}px`;
  const selected = STATE.selFile === f.path ? "selected" : "";

  return `
    <div class="side-row ${selected}" data-path="${esc(f.path)}" style="${pad}">
      ${xyPair(f.x, f.y)}${warnIcon(f)}
      <span class="label"><span class="p" title="${esc(f.path)}">${esc(f.path.split("/").pop())}</span></span>
      <span class="time">${pm || `<span title="${fmtSize(fs.size || 0)}">${relTime(fs.mtime || 0)}</span>`}</span>
      <span class="ops">${ops}</span>
    </div>`;
}

function renderFileList(files) {
  const box = $("#file-list");
  const keepScroll = box.scrollTop;  // 目录折叠/筛选/密度切换不打断滚动位置
  if (!files.length) {
    box.innerHTML = `<div class="empty"><h3>${STATE.filter ? "没有匹配的文件" : "工作区干净"}</h3>
      <p>${STATE.filter ? "换个关键字试试" : "没有待提交的改动"}</p></div>`;
    return;
  }
  loadExpandedDirs();

  box.innerHTML = "";
  const renderDirs = (node, prefix, depth) => {
    const dirs = [...node.dirs.entries()].sort((a, b) => a[0].localeCompare(b[0]));
    for (const [name, child] of dirs) {
      const full = prefix ? `${prefix}/${name}` : name;
      const expanded = STATE.filter ? true : STATE.expandedDirs.has(full);
      const el = document.createElement("div");
      el.className = "tree-dir";
      el.dataset.dir = full;
      el.style.paddingLeft = (8 + depth * 16) + "px";
      el.innerHTML = `<span class="caret">${expanded ? "▼" : "▶"}</span> ${esc(name)} <span class="cnt">(${countTreeFiles(child)})</span>`;
      box.appendChild(el);
      if (expanded) renderDirs(child, full, depth + 1);
    }
    for (const f of node.files.sort((a, b) => a.path.localeCompare(b.path))) {
      box.insertAdjacentHTML("beforeend", fileRowHtml(f, depth));
    }
  };

  // 分组：冲突 → 已暂存 → 未暂存（无头平铺）→ 未跟踪
  for (const [gid, label, list] of groupHcFiles(files)) {
    if (!gid) { renderDirs(buildTree(list), "", 0); continue; }  // 无组头组
    const collapsed = _groupCollapsed.has(gid) && !STATE.filter;
    const gplus = list.reduce((a, f) => a + (f.plus || 0), 0);
    const gminus = list.reduce((a, f) => a + (f.minus || 0), 0);
    const head = document.createElement("div");
    head.className = `wb-group ${gid}${collapsed ? " collapsed" : ""}`;
    head.dataset.group = gid;
    head.innerHTML = `<span class="caret">${collapsed ? "▶" : "▼"}</span> ${label}（${list.length}）` +
      (gplus || gminus ? `<span class="gpm"><span class="ns-plus">+${gplus}</span> <span class="ns-minus">-${gminus}</span></span>` : "");
    box.appendChild(head);
    if (!collapsed) renderDirs(buildTree(list), "", 0);
  }
  box.scrollTop = keepScroll;

  box.onclick = async (e) => {
    const grp = e.target.closest(".wb-group");
    if (grp) {
      const gid = grp.dataset.group;
      if (_groupCollapsed.has(gid)) _groupCollapsed.delete(gid);
      else _groupCollapsed.add(gid);
      renderFileList(currentFilteredFiles());
      return;
    }
    const dirEl = e.target.closest(".tree-dir");
    if (dirEl) {
      const full = dirEl.dataset.dir;
      if (STATE.expandedDirs.has(full)) STATE.expandedDirs.delete(full);
      else STATE.expandedDirs.add(full);
      persistExpandedDirs();
      renderFileList(currentFilteredFiles());
      return;
    }
    const btn = e.target.closest("button");
    const row = e.target.closest("[data-path]");
    if (btn) {
      e.stopPropagation();
      const file = btn.dataset.undo;
      const deleted = btn.dataset.deleted === "1";
      const ok = await confirmDialog({
        title: deleted ? "恢复已删除的文件？" : "放弃对文件的改动？",
        html: `<code>${esc(file)}</code><br><br>${deleted ? "从上次提交恢复该文件。" : "此操作<b>无法撤销</b>。"}`,
        confirmText: deleted ? "恢复" : "放弃改动",
        danger: !deleted,
      });
      if (!ok) return;
      await runBtn(btn, async () => {
        const d = await api("/api/git/restore", { method: "POST", body: { repo: repo(), file } });
        if (!d.ok) throw new Error(d.error || "操作失败");
        if (STATE.selFile === file) STATE.selFile = null;
        await refreshAll();
      });
      return;
    }
    if (row) {
      STATE.selFile = row.dataset.path;
      showView("#changes");
      renderSidebar();
      renderMain();
    }
  };
}

function currentFilteredFiles() {
  return (STATE.status?.files || []).filter((f) =>
    !STATE.filter || f.path.toLowerCase().includes(STATE.filter.toLowerCase()));
}

/** 新建分支：创建后询问是否立即切换（v3.1 #1） */
function createNewBranchFlow(sel) {
  $("#generic-title").textContent = "新建分支";
  $("#generic-body").innerHTML = `
    <div class="field"><label>分支名（不含空格和 ~^:?*[\\ 字符）</label>
      <input class="input" id="nb-name" placeholder="如 feature-x"></div>`;
  $("#generic-ok").textContent = "创建";
  openDialog("#generic-overlay");
  $("#generic-ok").onclick = () => runBtn($("#generic-ok"), async () => {
    const name = $("#nb-name").value.trim();
    if (!/^[^\s~^:?*\[\\]+$/.test(name)) {
      toast("分支名不能为空，且不能包含空格或 ~^:?*[\\ 字符", "error");
      return;
    }
    const d = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "create", name } });
    if (!d.ok) throw new Error(d.error || "创建失败");
    closeDialog("#generic-overlay");
    const sw = await confirmDialog({
      title: `已创建分支 ${name}`,
      html: "是否立即切换过去？",
      confirmText: "切换",
    });
    if (!sw) { renderSidebar(); return; }  // 刷新下拉列表
    const d2 = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "switch", name } });
    if (!d2.ok) throw new Error(d2.error || "切换失败");
    await refreshAll();
    toast(`已切换到 ${name}`, "success");
  });
}

function renderCommitList() {
  const box = $("#commit-list");
  const keepScroll = box.scrollTop;  // 重渲染不打断滚动位置（点提交列表跳顶 bug）
  if (!STATE.commits.length) {
    box.innerHTML = `<div class="empty"><h3>还没有提交</h3><p>提交后会显示在这里</p></div>`;
    return;
  }
  box.innerHTML = STATE.commits.map((c) => `
    <div class="side-row ${STATE.selCommit === c.hash ? "selected" : ""}" data-hash="${esc(c.hash)}">
      <span class="commit-hash">${esc(c.short)}</span>
      <span class="label"><span class="p">${esc(c.msg)}</span><span class="t">${esc(c.author)} · ${relTime(c.date)}</span></span>
    </div>`).join("");
  box.scrollTop = keepScroll;  // 恢复滚动位置

  box.onclick = (e) => {
    const row = e.target.closest("[data-hash]");
    if (row) {
      STATE.selCommit = row.dataset.hash;
      renderSidebar();
      renderMain();
    }
  };

  // 右键菜单：反向提交 / 建分支 / 清空历史 / 软回退 / 混合回退（+ 检出查看）
  box.oncontextmenu = (e) => {
    const row = e.target.closest("[data-hash]");
    if (!row) return;
    e.preventDefault();
    const hash = row.dataset.hash;
    showMenu([
      { label: "反向提交（生成撤销提交）", sub: "已推送也安全", onclick: () => runRollbackOp(hash, "revert") },
      { label: "在此提交上创建分支…", onclick: () => branchAtCommit(hash) },
      { label: "删除此提交", danger: true, sub: "只删这一条，其余原样保留 · 自动打保险 tag", onclick: () => deleteCommitOne(hash) },
      { divider: true },
      { label: "软回退到此提交", sub: "改动保留，重新提交", onclick: () => runRollbackOp(hash, "soft") },
      { label: "混合回退到此提交", sub: "改动保留在工作区", onclick: () => runRollbackOp(hash, "mixed") },
      { divider: true },
      { label: "检出此版本（查看，不改历史）", onclick: () => checkoutCommit(hash) },
    ], e.clientX, e.clientY);
  };
}

async function runRollbackOp(hash, mode) {
  try {
    const d = await api("/api/rollback/execute", { method: "POST", body: { repo: repo(), commit: hash, mode } });
    if (!d.ok) throw new Error(d.error || "操作失败");
    STATE.selCommit = null;
    await refreshAll();
    toast({ soft: "软回退完成，改动在暂存区", mixed: "混合回退完成，改动在工作区", revert: "反向提交已生成" }[mode] || "完成", "success");
  } catch (e) { toast(e.message, "error"); }
}

async function deleteCommitOne(hash) {
  const ok = await confirmDialog({
    title: "删除这条提交？",
    html: `只删除 <code>${esc(hash.slice(0, 7))}</code> 这一条，<b>其余提交原样保留</b>（时间重排，内容不变）。<br>删除前自动打保险 tag，可在「设置 → 紧急恢复」找回。<br>若删除会引发内容冲突，将自动恢复原状。`,
    confirmText: "删除",
    danger: true,
  });
  if (!ok) return;
  try {
    const d = await api("/api/git/commit_ops", { method: "POST", body: { repo: repo(), op: "delete_one", commit: hash } });
    if (!d.ok) throw new Error(d.error || "删除失败");
    STATE.selCommit = null;
    await refreshAll();
    toast(d.hint || "已删除该提交", "success");
  } catch (e) { toast(e.message, "error"); }
}

async function checkoutCommit(hash) {
  try {
    const d = await api("/api/git/commit_ops", { method: "POST", body: { repo: repo(), op: "checkout", commit: hash } });
    if (!d.ok) throw new Error(d.error || "检出失败");
    STATE.selFile = null;
    STATE.selCommit = null;
    await refreshAll();
    toast(d.hint || `已检出到 ${hash.slice(0, 7)}（游离 HEAD，浏览完从分支下拉切回去即可）`, "success");
  } catch (e) { toast(e.message, "error"); }
}

function branchAtCommit(hash) {
  $("#generic-title").textContent = `在 ${hash.slice(0, 7)} 上创建分支`;
  $("#generic-body").innerHTML = `
    <div class="field"><label>分支名（不含空格和 ~^:?*[\\ 字符）</label>
      <input class="input" id="ba-name" placeholder="如 hotfix-from-history"></div>
    <label class="check">
      <input type="checkbox" id="ba-switch"> 创建后立即切换过去
    </label>`;
  $("#generic-ok").textContent = "创建";
  openDialog("#generic-overlay");
  $("#generic-ok").onclick = () => runBtn($("#generic-ok"), async () => {
    const name = $("#ba-name").value.trim();
    if (!/^[^\s~^:?*\[\\]+$/.test(name)) {
      toast("分支名不能为空，且不能包含空格或 ~^:?*[\\ 字符", "error");
      return;
    }
    const doswitch = $("#ba-switch").checked;
    const d = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "create", name, start: hash } });
    if (!d.ok) throw new Error(d.error || "创建失败");
    closeDialog("#generic-overlay");
    if (doswitch) {
      const d2 = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "switch", name } });
      if (!d2.ok) throw new Error(d2.error || "切换失败");
      await refreshAll();
      toast(`已创建并切换到 ${name}`, "success");
    } else {
      await refreshAll();
      toast(`已在 ${hash.slice(0, 7)} 上创建分支 ${name}`, "success");
    }
  });
}

async function stashPopNow() {
  try {
    const d = await api("/api/git/branch", {
      method: "POST",
      body: { repo: repo(), action: "stash_pop" },
    });
    if (!d.ok) throw new Error(d.error || "恢复失败");
    await refreshAll();
    toast("已恢复储藏改动", "success");
  } catch (e) { toast(e.message, "error"); }
}

function bindChangesOps() {
  const s = STATE.status;
  // 进行中操作横幅（中止/继续）
  $("#ip-abort")?.addEventListener("click", async (e) => {
    await runBtn(e.target, async () => {
      const d = await api("/api/git/in_progress_op", { method: "POST", body: { repo: repo(), op: "abort" } });
      if (!d.ok) throw new Error(d.error || "中止失败");
      await refreshAll();
      toast("已中止，仓库恢复原状", "success");
    });
  });
  $("#ip-continue")?.addEventListener("click", async (e) => {
    await runBtn(e.target, async () => {
      const d = await api("/api/git/in_progress_op", { method: "POST", body: { repo: repo(), op: "continue" } });
      if (!d.ok) throw new Error(d.error || "继续失败");
      await refreshAll();
      toast("已完成", "success");
    });
  });
  // 冲突一键解决（用我方 / 用对方）
  $$(".cp-btns .btn").forEach((b) => {
    b.addEventListener("click", async (e) => {
      e.stopPropagation();
      await runBtn(b, async () => {
        const d = await api("/api/git/conflict_resolve", {
          method: "POST",
          body: { repo: repo(), file: b.dataset.cfile, side: b.dataset.cside },
        });
        if (!d.ok) throw new Error(d.error || "解决失败");
        await refreshAll();
        toast(d.hint || "冲突已解决", "success");
      });
    });
  });
  // 分支下拉
  const sel = $("#branch-select");
  api(`/api/git/branches?path=${encodeURIComponent(repo())}`)
    .then((d) => {
      let opts = d.branches.map((b) =>
        `<option value="${esc(b.name)}" ${b.current ? "selected" : ""}>${esc(b.name)}${b.upstream ? " ↑↓" : ""}</option>`).join("");
      // 游离 HEAD（查看历史版本）时，当前分支不存在于列表，插入提示项
      if (s?.branch && !d.branches.some((b) => b.name === s.branch)) {
        opts = `<option value="${esc(s.branch)}" selected>⊕ ${esc(s.branch)}（正在查看历史版本）</option>` + opts;
      }
      sel.innerHTML = `<option value="__new__">＋ 新建分支…</option>` + (opts || `<option value="${esc(s?.branch || "")}">${esc(s?.branch || "")}</option>`);
      if (s?.branch) sel.value = s.branch;  // 保持当前分支选中
      sel.onchange = async () => {
        if (sel.value === "__new__") { createNewBranchFlow(sel); return; }
        const name = sel.value;
        const ok = await confirmDialog({
          title: `切换到 ${name}？`,
          html: "如有未提交改动，会自动储藏后切换（稍后可点 [恢复储藏] 找回）。",
          confirmText: "切换",
        });
        if (!ok) { sel.value = s?.branch || ""; return; }
        const d2 = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "switch", name } }).catch((e) => ({ ok: false, error: e.message }));
        if (!d2.ok) { toast(d2.error, "error"); sel.value = s?.branch || ""; return; }
        await refreshAll();
        if (d2.hint) {
          // 切分支自动储藏了未提交改动：给一键找回入口（储藏不是提交，改动没丢）
          toast(`已切换到 ${name}，未提交改动已自动储藏`, "success",
            { label: "恢复改动", onclick: stashPopNow });
        } else {
          toast(`已切换到 ${name}`, "success");
        }
      };
    }).catch(() => { sel.innerHTML = `<option>${esc(s?.branch || "")}</option>`; });

  // 删除分支：删的是下拉框里选中的那个（克隆整仓库后清理不要的分支用）
  $("#branch-del").onclick = async () => {
    const name = sel.value;
    if (!name || name === "__new__") { toast("先在下拉框选中要删除的分支", "error"); return; }
    if (name === s?.branch) { toast("不能删除当前所在的分支，请先切换到别的分支", "error"); return; }
    const ok = await confirmDialog({
      title: `删除分支 ${name}？`,
      html: "只删除这个分支名，不影响其他分支和提交历史。",
      confirmText: "删除", danger: true,
    });
    if (!ok) return;
    let d = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "delete", name } })
      .catch((e) => ({ ok: false, error: e.message }));
    if (!d.ok && /not fully merged|未完全合并/i.test(d.error || "")) {
      const force = await confirmDialog({
        title: `${name} 还有提交没合并到当前分支`,
        html: "强制删除后，该分支独有的提交将不再显示在历史里（本地会保留一段时间，之后被 git 自动回收）。",
        confirmText: "强制删除", danger: true,
      });
      if (!force) return;
      d = await api("/api/git/branch", { method: "POST", body: { repo: repo(), action: "delete", name, force: true } })
        .catch((e) => ({ ok: false, error: e.message }));
    }
    if (!d.ok) { toast(d.error, "error"); return; }
    toast(`已删除分支 ${name}`, "success");
    _sidebarSig = "";  // 侧边栏签名不含分支列表，强制重建下拉框
    await refreshAll();
  };

  $("#file-filter").oninput = (e) => {
    STATE.filter = e.target.value;
    renderFileList(currentFilteredFiles());
  };

  $("#btn-commit")?.addEventListener("click", (e) => runBtn(e.target, async () => {
    const summary = $("#commit-summary").value.trim();
    if (!summary) { toast("请填写提交摘要", "error"); return; }
      const d = await api("/api/git/commit", {
        method: "POST",
        body: { repo: repo(), summary, description: $("#commit-desc")?.value || "" },
      });
      if (!d.ok) throw new Error(d.error || "提交失败");
      STATE.selFile = null;
      await refreshAll();
      if (STATE.auth.state === "logged_in") {
        toast("提交完成", "success", { label: "立即推送", onclick: () => $("#op-push")?.click() });
      } else {
        toast("提交完成（未推送，点顶栏 [推送] 上传）", "success");
      }
  }));
}

/* ---------- 主内容区 ---------- */

function showView(id) {
  if (location.hash === id) renderMain();
  else location.hash = id;
}

window.addEventListener("hashchange", renderMain);

let _mainSig = "";

function renderMain() {
  // 同视图 + 同选中 + 同数据代数 → 跳过，避免启动/刷新时重复请求（如 overview 拉两次）
  const view = location.hash || (repo() ? "#changes" : "#welcome");
  const sig = [view, STATE.selFile, STATE.selCommit, refreshGen].join("|");
  if (sig === _mainSig) return;
  _mainSig = sig;
  const main = $("#main");
  try {
    _renderMainInner(view, main);
  } catch (e) {
    // 渲染异常：显示可恢复的占位页而不是白屏（稳定 #6）
    console.error(e);
    reportFrontendError(`renderMain: ${e.message}`, e.stack);
    _mainSig = "";  // 清签名：数据到位后下一次渲染自动重试，错误页不会卡死
    $("#main").innerHTML = `
      <div class="main-hint">
        <h3>页面渲染出错</h3>
        <p>${esc(e.message)}</p>
        <button class="btn btn-primary" onclick="location.reload()">重新加载</button>
      </div>`;
  }
}

function _renderMainInner(view, main) {
  // 无仓库或欢迎/登录视图不需要侧边栏
  $("#sidebar").style.display =
    (view === "#welcome" || view === "#login" || !repo()) ? "none" : "";

  const fns = {
    "#welcome": viewWelcome, "#release": viewRelease,
    "#settings": viewSettings, "#login": viewLogin,
  };
  if (view === "#changes") {
    (STATE.selFile ? viewFileDetail : viewOverview)(main);
  } else if (view === "#history") {
    (STATE.selCommit ? viewCommitDetail : viewHistoryTable)(main);
  } else {
    (fns[view] || viewOverview)(main);
  }
}

function hint(main, title, text) {
  main.innerHTML = `<div class="main-hint"><h3>${esc(title)}</h3><p>${esc(text)}</p></div>`;
}

function fmtSize(n) {
  if (!n) return "—";
  if (n < 1024) return n + " B";
  if (n < 1048576) return (n / 1024).toFixed(1) + " KB";
  return (n / 1048576).toFixed(1) + " MB";
}

const XY_TEXT = { M: "已修改", A: "新文件", D: "已删除", R: "重命名", "??": "新文件" };

/* ----- 视图 A：仓库概览 ----- */

function viewOverview(main) {
  if (!repo()) { showView("#welcome"); return; }
  main.innerHTML = `<div class="view">
    <h1 class="view-title">仓库概览</h1>
    <div id="ov-body"><div class="card"><div class="empty">加载中…</div></div></div>
  </div>`;
  api(`/api/repo/overview?path=${encodeURIComponent(repo())}`).then((o) => {
    // 最近 7 天柱状图（纯 CSS）
    const days = [];
    for (let i = 6; i >= 0; i--) {
      const d = new Date(Date.now() - i * 86400000);
      const key = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
      const hit = (o.activity || []).find((a) => a.date === key);
      days.push({ key, count: hit ? hit.count : 0, label: `${d.getMonth() + 1}/${d.getDate()}` });
    }
    const max = Math.max(1, ...days.map((d) => d.count));
    const chart = days.map((d) => `
      <div class="bar-col">
        <div class="bar" style="height:${Math.max(2, Math.round(d.count / max * 64))}px" title="${d.key}：${d.count} 次提交"></div>
        <span>${d.label}</span>
      </div>`).join("");

    $("#ov-body").innerHTML = `
      <div class="ov-grid">
        <div class="card setting-block">
          <h3>基本信息</h3>
          <table class="kv">
            <tr><td>路径</td><td class="mono" title="${esc(o.path)}">${esc(o.path)}</td></tr>
            <tr><td>远程</td><td>${esc(o.remote || "未设置")}</td></tr>
            <tr><td>分支</td><td><b>${esc(o.branch)}</b></td></tr>
            <tr><td>与远程</td><td>${o.ahead ? `<span class="diff-count ahead">↑${o.ahead}</span> ` : ""}${o.behind ? `<span class="diff-count behind">↓${o.behind}</span>` : ""}${!o.ahead && !o.behind ? "同步" : ""}</td></tr>
            <tr><td>工作区</td><td>${o.workspace_files ? `${o.workspace_files} 个改动` : "干净"}</td></tr>
          </table>
        </div>
        <div class="card setting-block">
          <h3>改动统计</h3>
          <table class="kv">
            <tr><td>已修改</td><td>${o.changed_summary.M} 个文件</td></tr>
            <tr><td>新文件</td><td>${o.changed_summary.A} 个文件</td></tr>
            <tr><td>已删除</td><td>${o.changed_summary.D} 个文件</td></tr>
            <tr><td>重命名</td><td>${o.changed_summary.R} 个文件</td></tr>
            <tr><td>合计</td><td><span class="ns-plus">+${o.diff_total.plus}</span> <span class="ns-minus">-${o.diff_total.minus}</span></td></tr>
          </table>
        </div>
      </div>
      <div class="card setting-block" style="margin-top:12px">
        <h3>最近 7 天活动</h3>
        <div class="bar-chart">${chart}</div>
        <div class="help" style="margin-top:8px">总提交 ${o.total_commits} · 最后提交 ${o.last_commit_time ? relTime(o.last_commit_time) : "无"}</div>
      </div>`;
  }).catch((e) => {
    $("#ov-body").innerHTML = `<div class="error-box">${esc(e.message)}</div>`;
  });
}

/* ----- 视图 B：文件改动详情 ----- */

function viewFileDetail(main) {
  const file = STATE.selFile;
  main.innerHTML = `<div class="view">
    <h1 class="view-title">文件改动</h1>
    <div class="card" id="fd-body"><div class="empty">加载中…</div></div>
  </div>`;
  api(`/api/git/file_stats?path=${encodeURIComponent(repo())}&file=${encodeURIComponent(file)}`)
    .then((s) => {
      const deleted = s.xy.includes("D");
      $("#fd-body").innerHTML = `
        <div class="diff-header">
          ${xyPair(s.xy === "??" ? "?" : s.xy[0], s.xy === "??" ? "?" : s.xy[1] || ".")}
          <span class="path">${esc(s.file)}</span>
          ${s.xy !== "??" ? `<button class="btn btn-sm" id="d-undo" data-deleted="${deleted ? 1 : ""}">${deleted ? "恢复文件" : "撤销改动"}</button>` : ""}
        </div>
        <div class="setting-block">
          <table class="kv">
            <tr><td>状态</td><td>${XY_TEXT[s.xy] || esc(s.xy)}</td></tr>
            <tr><td>改动</td><td><span class="ns-plus">+${s.plus}</span> <span class="ns-minus">-${s.minus}</span></td></tr>
            <tr><td>大小</td><td>${fmtSize(s.size)}</td></tr>
            <tr><td>最后修改</td><td>${relTime(s.mtime)}</td></tr>
          </table>
        </div>
        <div style="padding:0 20px 20px">
          <details id="diff-fold">
            <summary>完整改动</summary>
            <div style="margin:8px 0">
              <button class="btn btn-sm" id="wd-toggle">逐词对比：关</button>
              <span class="help" style="font-size:11px">只高亮改动的那几个词，而不是整行</span>
            </div>
            <div class="diff-view" style="margin-top:10px"><div class="empty">加载中…</div></div>
          </details>
        </div>`;

      $("#d-undo")?.addEventListener("click", async (e) => {
        const ok = await confirmDialog({
          title: deleted ? "恢复已删除的文件？" : "放弃对文件的改动？",
          html: `<code>${esc(file)}</code><br><br>${deleted ? "从上次提交恢复该文件。" : "此操作<b>无法撤销</b>。"}`,
          confirmText: deleted ? "恢复" : "放弃改动",
          danger: !deleted,
        });
        if (!ok) return;
        runBtn(e.target, async () => {
          const d = await api("/api/git/restore", { method: "POST", body: { repo: repo(), file } });
          if (!d.ok) throw new Error(d.error || "操作失败");
          STATE.selFile = null;
          await refreshAll();
        });
      });

      // diff 直接加载；<300 行默认展开（v3.1 #4）；支持逐词对比开关
      let wd = STATE.wordDiff || false;
      const fold = $("#diff-fold");
      const wdBtn = $("#wd-toggle");
      wdBtn.textContent = `逐词对比：${wd ? "开" : "关"}`;
      const loadDiff = () => {
        api(`/api/git/diff?path=${encodeURIComponent(repo())}&file=${encodeURIComponent(file)}&word_diff=${wd ? 1 : 0}`)
          .then((d) => {
            fold.querySelector(".diff-view").innerHTML =
              wd ? renderWordDiffText(d.diff || "") : renderDiffText(d.diff || "");
            if ((d.lines || 0) < 300) {
              fold.open = true;
              fold.querySelector("summary").textContent = "完整改动";
            } else {
              fold.querySelector("summary").textContent = "完整改动（内容较长，点击展开）";
            }
          })
          .catch((e) => { fold.querySelector(".diff-view").innerHTML = `<div class="error-box">${esc(e.message)}</div>`; });
      };
      wdBtn.onclick = () => {
        wd = !wd;
        STATE.wordDiff = wd;
        wdBtn.textContent = `逐词对比：${wd ? "开" : "关"}`;
        loadDiff();
      };
      loadDiff();
    })
    .catch((e) => { $("#fd-body").innerHTML = `<div class="error-box">${esc(e.message)}</div>`; });
}

function renderDiffText(text, skipHeader = false) {
  if (!text?.trim()) return `<div class="empty"><h3>没有内容差异</h3><p>可能是新增的未跟踪文件，暂存后可查看</p></div>`;
  let lines = text.split("\n");
  if (skipHeader) {
    // 去掉第一个 @@ 之前的头部（diff --git / index / --- +++ 等元信息）
    const first = lines.findIndex((l) => l.startsWith("@@"));
    if (first > 0) lines = lines.slice(first);
  }
  return lines.map((line) => {
    const cls = line.startsWith("@@") ? "hunk"
      : line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ") ? "meta"
      : line.startsWith("+") ? "add"
      : line.startsWith("-") ? "del" : "";
    return `<div class="diff-line ${cls}">${esc(line) || " "}</div>`;
  }).join("");
}

/** 逐词对比渲染：[-旧-] {+新+} 行内高亮（v3.1 附加） */
function renderWordDiffText(text) {
  if (!text?.trim()) return `<div class="empty"><h3>没有内容差异</h3></div>`;
  return text.split("\n").map((line) => {
    const cls = line.startsWith("@@") ? "hunk"
      : line.startsWith("+++") || line.startsWith("---") || line.startsWith("diff ") || line.startsWith("index ") ? "meta" : "";
    let safe = esc(line);
    safe = safe.replace(/\[-(.*?)-\]/g, '<span class="wd-del">$1</span>');
    safe = safe.replace(/\{\+(.*?)\+\}/g, '<span class="wd-add">$1</span>');
    return `<div class="diff-line ${cls}">${safe || " "}</div>`;
  }).join("");
}

/* ----- 视图 C：提交详情（表格 + 行内 diff） ----- */

function viewCommitDetail(main) {
  main.innerHTML = `<div class="view view-wide cd-view">
    <div id="commit-detail" class="cd-wrap"><div class="empty">加载中…</div></div></div>`;
  api(`/api/git/commit_stats?path=${encodeURIComponent(repo())}&commit=${encodeURIComponent(STATE.selCommit)}`)
    .then((c) => {
      $("#commit-detail").innerHTML = `
        <div class="detail-head">
          <div style="display:flex;align-items:flex-start;gap:12px">
            <div style="flex:1;min-width:0">
              <h2>${esc(c.msg)}</h2>
              <div class="meta"><span class="mono">${esc(c.hash)}</span> · ${esc(c.author)} · ${esc(c.date)}（${relTime(c.date)}）</div>
            </div>
            <button class="btn" id="btn-rollback">回退到此提交</button>
          </div>
        </div>
        <div class="cd-3col">
          <div class="cd-files">
            <div class="cd-files-head">
              <span>改动的文件（${c.files.length}，<span class="ns-plus">+${c.total.plus}</span> <span class="ns-minus">-${c.total.minus}</span>）</span>
            </div>
            <div class="cd-file-list" id="cd-file-list"></div>
          </div>
          <div class="cd-preview" id="cd-preview"><div class="empty"><h3>点击左侧文件查看改动</h3><p>或点「全部改动」看整个提交的 patch</p></div></div>
        </div>`;
      $("#btn-rollback").onclick = () => openRollback(STATE.commits.findIndex((x) => x.hash === c.hash));

      const preview = $("#cd-preview");
      let selRow = null;
      let selPath = "";
      function setPreviewHead(text) {
        preview.innerHTML = `<div class="cd-preview-head" title="${esc(text)}">${esc(text)}</div>` +
          `<div class="cd-preview-body"><div class="empty">加载中…</div></div>`;
      }
      const cdExpanded = new Set(collectDirPaths(buildTree(c.files.map((f, i) => ({ ...f, _i: i }))), "", []));
      function cdTreeHtml(node, prefix) {
        let html = "";
        for (const [name, child] of node.dirs) {
          const full = prefix ? `${prefix}/${name}` : name;
          html += `<div class="cd-drow" data-dir="${esc(full)}">${esc(name)}/</div>`;
          if (cdExpanded.has(full)) html += cdTreeHtml(child, full);
        }
        for (const f of node.files) {
          html += `
            <div class="cd-frow ${selRow && c.files[+selRow.dataset.i]?.path === f.path ? "selected" : ""}" data-i="${f._i}" title="${esc(f.path)}">
              <span class="st-${esc(f.xy)}">${esc(f.xy)}</span>
              <span class="p">${esc(f.path.split("/").pop())}</span>
              <span class="ns"><span class="ns-plus">+${f.plus}</span> <span class="ns-minus">-${f.minus}</span></span>
            </div>`;
        }
        return html;
      }
      function renderCdFiles() {
        $("#cd-file-list").innerHTML = cdTreeHtml(buildTree(c.files.map((f, i) => ({ ...f, _i: i }))), "");
      }
      renderCdFiles();
      function loadPreview(url, headText) {
        setPreviewHead(headText || selPath);
        const body = preview.querySelector(".cd-preview-body");
        api(url)
          .then((r) => { body.innerHTML = `<div class="diff-view">${renderDiffText(r.diff || r.patch || "", true)}</div>`; })
          .catch((er) => { body.innerHTML = `<div class="error-box">${esc(er.message)}</div>`; });
      }
      function selectRow(row) {
        if (selRow) selRow.classList.remove("selected");
        selRow = row;
        row.classList.add("selected");
        selPath = c.files[+row.dataset.i].path;
        loadPreview(`/api/git/commit_file_diff?path=${encodeURIComponent(repo())}&commit=${encodeURIComponent(c.hash)}&file=${encodeURIComponent(selPath)}`, selPath);
      }
      $("#cd-file-list").onclick = (e) => {
        const dir = e.target.closest(".cd-drow");
        if (dir) {
          const full = dir.dataset.dir;
          if (cdExpanded.has(full)) cdExpanded.delete(full);
          else cdExpanded.add(full);
          renderCdFiles();
          return;
        }
        const row = e.target.closest(".cd-frow");
        if (row) selectRow(row);
      };
      // 默认选中第一个文件（Desktop 习惯：进来就能看到 diff）
      const first = $("#cd-file-list .cd-frow");
      if (first) selectRow(first);
    })
    .catch((e) => { $("#commit-detail").innerHTML = `<div class="error-box">${esc(e.message)}</div>`; });
}

/* ----- 视图 D：历史表格 ----- */

function viewHistoryTable(main) {
  main.innerHTML = `<div class="view view-wide"><h1 class="view-title">提交历史</h1>
    <div class="card" id="ht-body"></div></div>`;
  const rows = STATE.commits;
  if (!rows.length) {
    $("#ht-body").innerHTML = `<div class="empty"><h3>还没有提交</h3><p>提交后会显示在这里</p></div>`;
    return;
  }
  $("#ht-body").innerHTML = `
    <table class="ht-table">
      <thead><tr><th>Hash</th><th>消息</th><th>作者</th><th>时间</th><th>改动</th></tr></thead>
      <tbody>
        ${rows.map((c) => `
          <tr data-hash="${esc(c.hash)}">
            <td class="commit-hash">${esc(c.short)}</td>
            <td class="msg" title="${esc(c.msg)}">${esc(c.msg)}</td>
            <td>${esc(c.author)}</td>
            <td>${relTime(c.date)}</td>
            <td><span class="ns-plus">+${c.plus}</span> <span class="ns-minus">-${c.minus}</span></td>
          </tr>`).join("")}
      </tbody>
    </table>`;
  $("#ht-body").onclick = (e) => {
    const tr = e.target.closest("tr[data-hash]");
    if (!tr) return;
    STATE.selCommit = tr.dataset.hash;
    renderMain();
  };
}

/* ----- 回滚 ----- */

function openRollback(idx) {
  if (idx < 0) { toast("找不到该提交", "error"); return; }
  const c = STATE.commits[idx];
  const dropped = STATE.commits.slice(0, idx);
  const droppedHtml = dropped.length
    ? dropped.map((d) => `<li>${esc(d.short)} ${esc(d.msg)}</li>`).join("")
    : "<li>（无）</li>";

  $("#generic-title").textContent = `回退到 ${c.short}`;
  $("#generic-body").innerHTML = `
    <p style="margin-top:0"><b>${esc(c.msg)}</b><br>
      <span style="color:var(--text-2)">${esc(c.author)} · ${esc(c.date)} · <code>${esc(c.hash)}</code></span></p>
    <label class="check" style="margin-bottom:12px">
      <input type="checkbox" id="rb-pushed"> 该提交已推送到远程
    </label>
    <div id="rb-modes">${[
      { v: "soft", t: "软回退", d: "保留所有改动到暂存区，可立即重新提交", cmd: `git reset --soft ${c.short}` },
      { v: "mixed", t: "混合回退", d: "保留改动到工作区，但不暂存", cmd: `git reset --mixed ${c.short}` },
      { v: "hard", t: "硬回退 <span class='warn'>⚠ 危险</span>", d: "丢弃之后的所有提交和改动，无法通过普通操作恢复", cmd: `git reset --hard ${c.short}`, danger: true },
      { v: "revert", t: "反向提交", d: "新建一个提交来抵消目标提交，历史保留（已推送的提交推荐用这个）", cmd: `git revert --no-edit ${c.short}` },
    ].map((m, i) => `
      <label class="radio-card ${m.danger ? "danger" : ""} ${i === 0 ? "checked" : ""}">
        <input type="radio" name="rb-mode" value="${m.v}" ${i === 0 ? "checked" : ""}>
        <span><span class="t">${m.t}</span><span class="d">${m.d}</span><span class="cmd">${m.cmd}</span></span>
      </label>`).join("")}</div>`;
  $("#rb-modes").addEventListener("change", () => {
    $$("#rb-modes .radio-card").forEach((r) => r.classList.toggle("checked", r.querySelector("input").checked));
  });

  $("#generic-ok").textContent = "下一步";
  $("#generic-ok").onclick = async () => {
    const mode = document.querySelector("input[name=rb-mode]:checked").value;
    if (mode === "hard") {
      const ok = await confirmDialog({
        title: "确认执行危险操作？",
        html: `你将执行：<code>git reset --hard ${esc(c.short)}</code><br><br>
          <b>这会丢弃：</b><ul>${droppedHtml}</ul>
          ${STATE.status?.files.length ? `<p>以及工作区 <b>${STATE.status.files.length}</b> 个未提交的改动</p>` : ""}
          <b>保险措施：</b>执行前会自动打一个 tag：<code>_backup_日期_时间</code>。<br>
          如需找回：设置 → 紧急恢复 → 选择该 tag。`,
        confirmText: "我确认，执行", danger: true,
      });
      if (!ok) return;
    }
    const okBtn = $("#generic-ok");
    okBtn.disabled = true;
    okBtn.textContent = "执行中…";
    try {
      const d = await api("/api/rollback/execute", {
        method: "POST", body: { repo: repo(), commit: c.hash, mode },
      });
      if (!d.ok) throw new Error(d.error || "回退失败");
      closeDialog("#generic-overlay");
      if (mode === "hard" && d.backup_tag) showBackupBanner(d.backup_tag);
      else toast(mode === "revert" ? "已生成反向提交" : "回退完成", "success");
      await refreshAll();
    } catch (e) {
      toast(e.message, "error");
      okBtn.disabled = false;
      okBtn.textContent = "下一步";
    }
  };
  openDialog("#generic-overlay");
}

function showBackupBanner(tag) {
  $("#banner-slot")?.remove();
  const el = document.createElement("div");
  el.className = "banner";
  el.id = "banner-slot";
  el.innerHTML = `<span>⚠ 已执行硬回退。保险 tag：<span class="mono">${esc(tag)}</span>。如需找回：设置 → 紧急恢复 → 选择该 tag。</span>
    <button class="btn btn-sm">知道了</button>`;
  el.querySelector("button").onclick = () => el.remove();
  $("#topbar").after(el);
}

/* ----- Release 视图 ----- */

function githubWebUrl(remote, tag) {
  if (!remote?.startsWith("github.com/")) return "";
  return `https://${remote}/releases/tag/${encodeURIComponent(tag)}`;
}

function viewRelease(main) {
  main.innerHTML = `
    <div class="view">
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:16px">
        <h1 class="view-title" style="margin:0">Release</h1>
        <button class="btn btn-primary" id="btn-new-release">＋ 新建 Release</button>
      </div>
      <div id="release-list"><div class="empty">加载中…</div></div>
    </div>`;
  loadReleases();

  $("#btn-new-release").onclick = () => {
    $("#generic-title").textContent = "新建 Release";
    $("#generic-body").innerHTML = `
      <div class="field"><label>Tag（如 v1.2.3）</label><input class="input" id="rel-tag"></div>
      <div class="field"><label>标题</label><input class="input" id="rel-title"></div>
      <div class="field"><label>说明（Markdown）</label><textarea class="input" id="rel-notes"></textarea></div>
      <div class="field"><label>附件（可多选）</label>
        <div class="form-row">
          <input class="input" id="rel-files" placeholder="点击右侧按钮选择文件" disabled>
          <button class="btn" id="rel-pick">选择文件</button>
        </div>
      </div>`;
    $("#generic-ok").textContent = "发布";
    $("#generic-ok").onclick = () => runBtn($("#generic-ok"), async () => {
      const tag = $("#rel-tag").value.trim();
      if (!tag) { toast("请填写 tag", "error"); return; }
      const files = $("#rel-files").value.split(" | ").filter(Boolean);
      const d = await api("/api/gh/release", {
        method: "POST",
        body: { tag, files, title: $("#rel-title").value, notes: $("#rel-notes").value, repo: repo() },
      });
      closeDialog("#generic-overlay");
      toast(`Release 已发布 ${d.url || ""}`, "success");
      loadReleases();
    });
    openDialog("#generic-overlay");
    $("#rel-pick").onclick = () => pickFiles((ps) => { $("#rel-files").value = ps.join(" | "); });
  };
}

function loadReleases() {
  const box = $("#release-list");
  if (!box) return;
  api(`/api/gh/releases?path=${encodeURIComponent(repo())}`)
    .then((d) => {
      box.innerHTML = d.releases.length
        ? `<div class="card">${d.releases.map((r) => {
            const remote = STATE.repos.find((x) => x.path === repo())?.remote || "";
            const webUrl = githubWebUrl(remote, r.tag);
            return `
            <div class="release-row">
              <div class="head">
                <span class="tag">${esc(r.tag)}</span>
                <span class="title">${esc(r.title || "")}</span>
                ${r.latest ? `<span class="badge badge-g">Latest</span>` : ""}
                <span class="help" style="font-size:11px">${esc(r.date || "")}</span>
                ${webUrl ? `<a class="btn btn-sm" href="${webUrl}" target="_blank">查看</a>` : ""}
                <button class="btn btn-sm" data-del="${esc(r.tag)}">删除</button>
              </div>
            </div>`;
          }).join("")}</div>`
        : `<div class="empty"><h3>还没有 Release</h3><p>点右上角 [＋ 新建 Release] 发布第一个版本</p></div>`;

      box.onclick = async (e) => {
        const btn = e.target.closest("[data-del]");
        if (!btn) return;
        const ok = await confirmDialog({
          title: `删除 Release ${btn.dataset.del}？`,
          html: "远程的 Release 会被删除，已上传的附件一并删除。本地的 tag 不受影响。",
          confirmText: "删除", danger: true,
        });
        if (!ok) return;
        await runBtn(btn, async () => {
          await api("/api/gh/release_delete", { method: "POST", body: { tag: btn.dataset.del, repo: repo() } });
          toast("已删除", "success");
          loadReleases();
        });
      };
    })
    .catch((e) => { box.innerHTML = `<div class="error-box">${esc(e.message)}</div>`; });
}

/* ----- 欢迎视图 ----- */

function viewWelcome(main) {
  main.innerHTML = `
    <div class="welcome">
      <h1>本地 Git 与 GitHub 的桌面控制台</h1>
      <p>克隆、提交、推送、回滚、发布 Release。本地操作无需登录。</p>
      <div class="btn-row">
        <button class="btn btn-primary" id="w-open">打开本地仓库</button>
        <button class="btn" id="w-clone">克隆仓库</button>
        <button class="btn" id="w-new">新建仓库</button>
      </div>
    </div>`;
  $("#w-open").onclick = openLocalRepo;
  $("#w-clone").onclick = openCloneDialog;
  $("#w-new").onclick = openNewRepoWizard;
}

/* ----- 打开 / 克隆 / 新建仓库 ----- */

async function openLocalRepo() {
  const input = { value: "" };
  await pickFolder(input);
  const path = input.value;
  if (!path) return;
  try {
    const d = await api("/api/repo/open", { method: "POST", body: { path } });
    // 先判非 git 目录（ok:true 但没有 repo），再判成功打开
    if (d.is_git_repo === false) {
      const ok = await confirmDialog({
        title: "该目录不是 Git 仓库",
        html: `是否在 <code>${esc(path)}</code> 初始化 Git 仓库？`,
        confirmText: "初始化",
      });
      if (!ok) return;
      const r = await api("/api/repo/init", { method: "POST", body: { path } });
      if (!r.ok) throw new Error(r.error);
      await afterRepoReady(r.repo.path);
      return;
    }
    if (d.ok && d.repo) {
      await afterRepoReady(d.repo.path);
      return;
    }
    throw new Error(d.error || "打开失败");
  } catch (e) { toast(e.message, "error"); }
}

async function afterRepoReady(path) {
  await loadRepos();
  // 后端仓库列表存的是 normpath 规范化路径；克隆等流程传进来的可能是正斜杠/
  // 大小写不同的写法。先归一化匹配到列表里的规范路径，否则顶栏 find(===) 失败，
  // 下拉框显示不出仓库名（要再点一下才正常）
  const norm = (p) => String(p).replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
  const hit = STATE.repos.find((r) => norm(r.path) === norm(path));
  path = hit ? hit.path : path;
  // 先清掉上一个仓库的瞬态状态，防止切换窗口期用旧数据渲染新仓库
  STATE.active = path;
  api("/api/repo/activate", { method: "POST", body: { path } }).catch(() => {});  // 持久化：下次启动回到这个仓库
  STATE.status = null;
  STATE.commits = [];
  STATE.selFile = null;
  STATE.selCommit = null;
  STATE.filter = "";
  STATE.expandedDirs = new Set();
  _dirsRepo = "";  // 强制 loadExpandedDirs 重新读该仓库的展开记忆
  _mainSig = "";  // 强制下次渲染重建
  renderAll();
  await refreshAll();
  showView("#changes");
}

function openCloneDialog() {
  $("#generic-title").textContent = "克隆仓库";
  $("#generic-body").innerHTML = `
    <div class="field"><label>仓库地址（只需粘贴 URL，支持 gh-proxy 等中转链接）</label>
      <input class="input" id="clone-url" placeholder="https://github.com/user/repo.git"></div>
    <div class="field"><label>保存位置</label>
      <div class="form-row">
        <input class="input" id="clone-path" placeholder="选择或输入本地目录" value="${esc(STATE.lastCloneDir || "")}">
        <button class="btn" id="clone-browse">浏览</button>
      </div></div>
    <label class="check"><input type="checkbox" id="clone-history" checked> 所有历史版本（不勾 = 每个分支只下最新版本，省流量）</label>
    <label class="check"><input type="checkbox" id="clone-branches" checked> 所有分支（不勾 = 只下载默认分支）</label>
    ${STATE.lastCloneDir ? `<div class="help" style="font-size:11px;margin-top:4px">上次用的位置已填好，想换就重新选或改</div>` : ""}`;
  $("#generic-ok").textContent = "开始克隆";
  openDialog("#generic-overlay");
  $("#clone-browse").onclick = () => pickFolder($("#clone-path"));

  $("#generic-ok").onclick = async () => {
    const url = $("#clone-url").value.trim();
    const path = $("#clone-path").value.trim();
    if (!url || !path) { toast("请填写仓库地址和保存位置", "error"); return; }
    STATE.lastCloneDir = path;  // 换了位置就记新的，不用重启也生效
    startClone(url, path, $("#clone-history").checked, $("#clone-branches").checked);
  };
}

function startClone(url, path, allHistory, allBranches) {
  api("/api/repo/clone", {
    method: "POST",
    body: { url, path, all_history: allHistory, all_branches: allBranches },
  }).then((d) => {
    closeDialog("#generic-overlay");
    pollClone(d.clone_id, url);
  }).catch((e) => {
    toast(e.message, "error");
    const okBtn = $("#generic-ok");
    if (okBtn) { okBtn.disabled = false; okBtn.textContent = "开始克隆"; }
  });
}

function pollClone(id, url) {
  $("#generic-title").textContent = "克隆进度";
  $("#generic-body").innerHTML = `
    <p style="margin-top:0">正在克隆 <code>${esc(url)}</code></p>
    <div class="progress-box" id="clone-log">连接中…</div>`;
  $("#generic-ok").textContent = "后台运行";
  $("#generic-ok").onclick = () => closeDialog("#generic-overlay");
  openDialog("#generic-overlay");
  const logEl = $("#clone-log");

  const timer = setInterval(async () => {
    try {
      const d = await api(`/api/repo/clone_progress?id=${id}`);
      logEl.textContent = d.lines.join("\n") || "…";
      logEl.scrollTop = logEl.scrollHeight;
      if (d.done) {
        clearInterval(timer);
        if (d.success) {
          logEl.textContent += "\n✔ 克隆完成";
          toast("克隆完成", "success");
          await loadRepos();
          await afterRepoReady(d.repo_path);
          closeDialog("#generic-overlay");
        } else {
          logEl.textContent += "\n✘ 克隆失败";
          // 后端失败时会在日志里写一条 HINT: 开头的中文提示（网络/认证/地址问题）
          const hintLine = (d.lines || []).find((l) => l.startsWith("HINT:"));
          toast(hintLine ? hintLine.slice(5) : "克隆失败，详见日志", "error");
          $("#generic-ok").textContent = "关闭";
        }
      }
    } catch { /* 下轮再试 */ }
  }, 500);
}

function openNewRepoWizard() {
  $("#generic-title").textContent = "新建仓库";
  $("#generic-body").innerHTML = `
    <div class="wizard-step">第一步 · 本地位置</div>
    <div class="form-row">
      <input class="input" id="nr-path" placeholder="选择本地目录">
      <button class="btn" id="nr-browse">浏览</button>
    </div>
    <div class="wizard-step">第二步 · GitHub 信息</div>
    <div class="field"><label>仓库名（默认 = 文件夹名）</label><input class="input" id="nr-name"></div>
    <label class="check" style="margin-bottom:10px">
      <input type="checkbox" id="nr-remote" checked> 同时创建 GitHub 远程仓库并推送
    </label>
    <div class="field" id="nr-remote-fields">
      <label>可见性</label>
      <label class="check"><input type="radio" name="nr-vis" value="public" checked> Public（公开）</label>
      <label class="check"><input type="radio" name="nr-vis" value="private"> Private（私有）</label>
    </div>`;
  $("#generic-ok").textContent = "创建";
  openDialog("#generic-overlay");

  $("#nr-browse").onclick = () => pickFolder($("#nr-path"));
  $("#nr-remote").onchange = (e) => {
    $("#nr-remote-fields").style.display = e.target.checked ? "" : "none";
  };

  $("#generic-ok").onclick = () => runBtn($("#generic-ok"), async () => {
    const path = $("#nr-path").value.trim();
    if (!path) { toast("请选择本地目录", "error"); return; }
    const createRemote = $("#nr-remote").checked;
    const name = $("#nr-name").value.trim() || path.replace(/[\\/]+$/, "").split(/[\\/]/).pop();
    const body = {
      path, name, create_remote: createRemote,
      private: document.querySelector("input[name=nr-vis]:checked")?.value === "private",
    };
    const d = await api("/api/repo/init_and_publish", { method: "POST", body });
    if (!d.ok) throw new Error((d.hint ? d.hint + "\n\n" : "") + (d.error || "创建失败"));
    closeDialog("#generic-overlay");
    toast(createRemote ? "仓库已创建并推送到 GitHub" : "本地仓库已创建", "success");
    await afterRepoReady(d.repo.path);
  });
}

/* ----- 设置视图 ----- */

function viewSettings(main) {
  main.innerHTML = `
    <div class="view">
      <h1 class="view-title">设置</h1>
      <div class="card">
        <div class="setting-block">
          <h3>账号</h3>
          <div id="account-state" style="display:flex;align-items:center;gap:10px;min-height:28px">
            <span class="help">正在检查登录状态…</span>
          </div>
        </div>
        <div class="setting-block">
          <h3>代理</h3>
          <p class="help">联网操作会通过这里配置的代理进行，改完保存后下次调用即生效</p>
          <label class="check" style="margin-bottom:12px">
            <input type="checkbox" id="proxy-enabled"> 启用代理
          </label>
          <div class="field"><label>代理地址</label><input class="input" id="proxy-url" placeholder="http://127.0.0.1:31181"></div>
          <div class="btn-row">
            <button class="btn btn-primary" id="save-proxy">保存</button>
            <button class="btn" id="test-proxy">测试连接</button>
          </div>
        </div>
        <div class="setting-block">
          <h3>编辑器</h3>
          <p class="help">默认代码编辑器的路径（可选）</p>
          <div class="form-row" style="max-width:480px">
            <input class="input" id="editor-path" placeholder="如 C:\\Program Files\\VS Code\\Code.exe">
          </div>
          <button class="btn" id="save-editor" style="margin-top:8px">保存</button>
        </div>
        <div class="setting-block">
          <h3>超时</h3>
          <p class="help">git / gh 命令超时时间（秒），防止界面假死</p>
          <div class="form-row" style="max-width:240px">
            <input class="input" type="number" id="git-timeout" min="30" max="3600">
            <button class="btn" id="save-timeout">保存</button>
          </div>
        </div>
        <div class="setting-block">
          <h3>紧急恢复</h3>
          <p class="help">硬回退前会自动打保险 tag；这里可列出所有可恢复点并一键恢复（针对当前仓库）</p>
          <button class="btn" id="load-recovery" ${repo() ? "" : "disabled"}>加载恢复点</button>
          <div id="recovery-panel">
            <div style="margin-top:14px">
              <h3 style="font-size:13px">保险 tag</h3>
              <div class="card" id="backup-list"></div>
            </div>
            <div style="margin-top:14px">
              <h3 style="font-size:13px">reflog（兜底）</h3>
              <div class="card" id="reflog-list"></div>
            </div>
          </div>
        </div>
        <div class="setting-block">
          <h3>关于</h3>
          <p class="help" id="about">读取版本信息中…</p>
        </div>
      </div>
    </div>`;
  bindSettings();
}

function bindSettings() {
  api("/api/config").then(({ config }) => {
    $("#proxy-enabled").checked = config.proxy.enabled;
    $("#proxy-url").value = config.proxy.url;
    $("#git-timeout").value = config.git.cmd_timeout;
    $("#editor-path").value = config.editor || "";
  });

  renderAccountBlock();

  $("#save-proxy").onclick = (e) => runBtn(e.target, async () => {
    await api("/api/config", { method: "POST", body: {
      proxy: { enabled: $("#proxy-enabled").checked, url: $("#proxy-url").value.trim() } } });
    toast("代理设置已保存", "success");
  });
  $("#test-proxy").onclick = (e) => runBtn(e.target, async () => {
    await api("/api/config", { method: "POST", body: {
      proxy: { enabled: $("#proxy-enabled").checked, url: $("#proxy-url").value.trim() } } });
    const d = await api("/api/config/proxy_test", { method: "POST" });
    if (d.ok && d.gh_ok) toast("代理可用，gh 联网正常", "success");
    else toast(d.error || "代理不可用", "error");
  });
  $("#save-editor").onclick = (e) => runBtn(e.target, async () => {
    await api("/api/config", { method: "POST", body: { editor: $("#editor-path").value.trim() } });
    toast("已保存", "success");
  });
  $("#save-timeout").onclick = (e) => runBtn(e.target, async () => {
    await api("/api/config", { method: "POST", body: { git: { cmd_timeout: +$("#git-timeout").value || 300 } } });
    toast("已保存", "success");
  });

  $("#load-recovery").onclick = (e) => runBtn(e.target, async () => {
    const d = await api(`/api/rollback/recovery?path=${encodeURIComponent(repo())}`);
    $("#backup-list").innerHTML = d.backups.length
      ? d.backups.map((b) => `<div class="file-stat" style="font-family:var(--font);font-size:13px;display:flex;gap:10px;align-items:center">
          <span class="badge badge-b">${esc(b.tag)}</span>
          <span class="mono">${esc(b.hash)}</span>
          <span style="color:var(--text-2);font-size:12px;flex:1">${esc(b.date)}</span>
          <button class="btn btn-sm" data-rec="${esc(b.hash)}">恢复</button></div>`).join("")
      : `<div class="empty">还没有保险 tag（硬回退时自动创建）</div>`;
    $("#reflog-list").innerHTML = d.reflog.length
      ? d.reflog.map((m) => `<div class="file-stat" style="font-family:var(--font);font-size:13px;display:flex;gap:10px;align-items:center">
          <span class="mono">${esc(m.hash)}</span>
          <span style="flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${esc(m.action)}">${esc(m.action)}</span>
          <button class="btn btn-sm" data-rec="${esc(m.hash)}">恢复</button></div>`).join("")
      : `<div class="empty">没有 reflog 记录</div>`;

    $("#recovery-panel").onclick = async (e) => {
      const btn = e.target.closest("[data-rec]");
      if (!btn) return;
      const ok = await confirmDialog({
        title: "恢复到该提交？",
        html: "将执行 <code>git reset --hard</code>。当前未提交的改动请先确认已保存，当前 HEAD 不会自动备份。",
        confirmText: "恢复", danger: true,
      });
      if (!ok) return;
      await runBtn(btn, async () => {
        await api("/api/rollback/recover", { method: "POST", body: { repo: repo(), commit: btn.dataset.rec } });
        toast("已恢复", "success");
        await refreshAll();
      });
    };
  });

  api("/api/config/versions").then((v) => {
    $("#about").textContent = `${v.git} | ${v.gh}`;
  }).catch(() => { $("#about").textContent = "无法读取 git/gh 版本"; });
}

function renderAccountBlock() {
  const box = $("#account-state");
  if (STATE.auth.state === "logged_in") {
    box.innerHTML = `<span class="badge badge-g">已登录：${esc(STATE.auth.username)}</span>
      <button class="btn btn-sm" id="btn-logout">登出</button>`;
    $("#btn-logout").onclick = async (e) => runBtn(e.target, async () => {
      await api("/api/auth/logout", { method: "POST" });
      STATE.auth = { state: "not_logged_in", username: "" };
      renderAuthChip();
      applyAuthLocks();
      renderAccountBlock();
      toast("已登出", "success");
    });
  } else if (STATE.auth.state === "expired") {
    box.innerHTML = `<span class="badge badge-y">登录已过期</span> <a href="#login">重新登录</a>`;
  } else {
    box.innerHTML = `<span class="badge badge-k">未登录</span> <a href="#login">去登录</a>`;
  }
}

/* ----- 登录视图 ----- */

function viewLogin(main) {
  main.innerHTML = `
    <div class="view" style="max-width:520px">
      <h1 class="view-title">登录 GitHub</h1>
      <form class="card setting-block" id="login-form">
        <div class="field">
          <label>Personal Access Token</label>
          <input class="input" type="password" id="token" placeholder="ghp_…" autocomplete="off" required>
          <span class="help">
            获取方式：GitHub → Settings → Developer settings → Personal access tokens →
            Tokens (classic) → Generate new token，勾选 <code>repo</code>、<code>read:org</code> 权限。
            <a href="https://github.com/settings/tokens/new?scopes=repo,read:org" target="_blank">点此直达创建页</a>
          </span>
        </div>
        <button class="btn btn-primary" type="submit">登录</button>
      </form>
      <p class="help" style="margin-top:14px">
        说明：GitHub 已于 2021 年禁用账号密码认证，PAT 是唯一官方支持的登录方式。
        本地操作（打开仓库、提交、回滚）无需登录。
      </p>
    </div>`;
  const prev = location.hash;
  $("#login-form").onsubmit = async (e) => {
    e.preventDefault();
    const btn = $("#login-form button[type=submit]");
    await runBtn(btn, async () => {
      const d = await api("/api/auth/login", { method: "POST", body: { token: $("#token").value } });
      if (!d.ok) throw new Error(d.error || "登录失败");
      STATE.auth = { state: "logged_in", username: d.username || "" };
      renderAuthChip();
      applyAuthLocks();
      toast(`登录成功，欢迎 ${d.username}`, "success");
      setTimeout(() => showView(prev === "#login" || !repo() ? "#welcome" : "#changes"), 600);
    });
  };
}

/* ---------- 数据加载与刷新 ---------- */

async function loadRepos() {
  const d = await api("/api/repo/list");
  STATE.repos = d.repositories;
  STATE.active = d.active && STATE.repos.some((r) => r.path === d.active) ? d.active : "";
}

let refreshGen = 0;

/* 硬核状态（porcelain=v2 全字段 + 逐文件 +/- + fs 探测），并派生旧版兼容结构 */
function applyHardcore(hc) {
  STATE.hc = hc;
  const m = hc.meta || {};
  STATE.status = {
    branch: m.branch || "",
    ahead: m.ahead || 0,
    behind: m.behind || 0,
    remote: hc.remote || "",
    // 兼容旧消费者：附带 xy 单字段，但保留 x/y/fs/plus/minus 全部硬核字段
    files: hc.files.map((f) => ({
      ...f,
      xy: f.x === "?" ? "??" : (f.conflict || ((f.x === "." ? "" : f.x) + (f.y === "." ? "" : f.y)) || "."),
    })),
  };
}

async function refreshAll() {
  if (!repo()) { renderAll(); return; }
  const gen = ++refreshGen;
  // status / log 并行请求，各自到达即渲染（历史/文件不用互相等 → 消除延迟感）
  const applyStatus = api(`/api/repo/status_hardcore?path=${encodeURIComponent(repo())}`)
    .then((st) => {
      if (gen !== refreshGen) return;
      applyHardcore(st);
      syncRemoteFromStatus({ remote: st.remote || "" });
      renderAll();
    })
    .catch((e) => {
      if (gen !== refreshGen) return;
      STATE.status = null;
      STATE.hc = null;
      toast(e.message, "error");
      renderAll();
    });
  const applyLog = api(`/api/git/log_table?path=${encodeURIComponent(repo())}&n=50`)
    .then((lc) => {
      if (gen !== refreshGen) return;
      STATE.commits = lc.commits.map((c) => ({ ...c }));
      renderAll();
    })
    .catch(() => { if (gen === refreshGen) renderAll(); });
  await Promise.all([applyStatus, applyLog]);
}

/* 窗口重新聚焦时自动刷新状态（外部工具改了仓库，鲁棒 #2），节流 3 秒 */
let lastFocusRefresh = 0;
window.addEventListener("focus", () => {
  if (!repo()) return;
  const now = Date.now();
  if (now - lastFocusRefresh < 3000) return;
  lastFocusRefresh = now;
  api(`/api/repo/status_hardcore?path=${encodeURIComponent(repo())}`)
    .then((st) => { applyHardcore(st); renderSidebar(); })
    .catch(() => {});
});

let _topbarSig = "";
let _sidebarSig = "";

function renderAll() {
  // 拆"重建"和"更新"：签名没变就不整体重写 DOM（性能 2-4）
  const tSig = `${STATE.auth.state}|${STATE.auth.username}|${repo()}`;
  if (tSig !== _topbarSig) { _topbarSig = tSig; renderTopbar(); }
  const sSig = JSON.stringify([STATE.tab, STATE.status?.branch,
    (STATE.status?.files || []).map((f) => f.path).join(","), STATE.selFile,
    STATE.commits.length, STATE.commits[0]?.hash]);
  if (sSig !== _sidebarSig) { _sidebarSig = sSig; renderSidebar(); }
  renderMain();
}

/* ---------- 窗口状态 ---------- */

// pywebview 的 js_api 是异步的，beforeunload 里的调用会在关窗时被丢弃。
// 改成定时轮询：值变了才发请求，关窗前最多丢 2 秒内的变化。
let lastWinSig = "";
setInterval(() => {
  try {
    if (document.hidden) return;  // 最小化时 screenX 会变成 -32000，不保存
    const sig = `${outerWidth}x${outerHeight}+${screenX}+${screenY}`;
    if (sig !== lastWinSig) {
      lastWinSig = sig;
      native()?.save_window_state?.(outerWidth, outerHeight, screenX, screenY);
    }
  } catch { /* 忽略 */ }
}, 2000);

/* ---------- 初始化 ---------- */

document.addEventListener("DOMContentLoaded", async () => {
  // 公共 DOM：对话框 + 菜单 + toast 容器
  document.body.insertAdjacentHTML("beforeend", `
    <div class="overlay" id="confirm-overlay">
      <div class="dialog">
        <header id="confirm-title"></header>
        <div class="body" id="confirm-body"></div>
        <footer>
          <button class="btn" id="confirm-cancel">取消</button>
          <button class="btn btn-primary" id="confirm-ok">确认</button>
        </footer>
      </div>
    </div>
    <div class="overlay" id="generic-overlay">
      <div class="dialog">
        <header id="generic-title"></header>
        <div class="body" id="generic-body"></div>
        <footer><button class="btn btn-primary" id="generic-ok">确定</button></footer>
      </div>
    </div>
    <div class="dropdown" id="menu"></div>
    <div id="progress"></div>
    <div id="toasts"></div>`);

  renderTopbar();
  renderMain();

  try {
    const [auth, repos, cfg] = await Promise.all([
      api("/api/auth/status"),
      api("/api/repo/list"),
      api("/api/config"),
    ]);
    STATE.auth = { state: auth.state, username: auth.username || "" };
    STATE.repos = repos.repositories;
    STATE.active = repos.active && STATE.repos.some((r) => r.path === repos.active)
      ? repos.active : "";
    STATE.uiExpanded = cfg.config?.ui?.expanded_dirs || {};
    STATE.lastCloneDir = cfg.config?.ui?.last_clone_dir || "";
    if (STATE.active) {
      // 启动只等状态接口（首屏所需的全部数据）；
      // 提交历史放后台加载，就绪后自动补渲染，首屏更快可交互
      try {
        const st = await api(`/api/repo/status_hardcore?path=${encodeURIComponent(STATE.active)}`);
        applyHardcore(st);
        syncRemoteFromStatus({ remote: st.remote || "" });
        api(`/api/git/log_table?path=${encodeURIComponent(STATE.active)}&n=50`)
          .then((lc) => {
            STATE.commits = lc.commits.map((c) => ({ ...c }));
            renderAll();
          })
          .catch(() => {});
      } catch {
        toast("上次打开的仓库已不存在", "error");
        STATE.active = "";
      }
    }
  } catch (e) {
    toast(e.message, "error");
  }

  renderAll();
  if (repo()) showView("#changes"); else showView("#welcome");

  // 启动埋点：页面导航起点到界面可交互的耗时（performance.now 以导航为 0 点）
  try {
    api("/api/client_log", {
      method: "POST",
      body: { msg: `boot: ui interactive +${Math.round(performance.now())}ms (repo=${repo() ? "yes" : "no"})` },
    });
  } catch {}

  // 后台异步真实验证登录态（乐观值错了再更新 UI），不阻塞启动（v3.1 #3 第三层）
  api("/api/auth/verify").then((v) => {
    if (v.state !== STATE.auth.state || (v.username || "") !== STATE.auth.username) {
      STATE.auth = { state: v.state, username: v.username || "" };
      renderAuthChip();
      applyAuthLocks();
      if ($("#account-state")) renderAccountBlock();
    }
  }).catch(() => {});
});
