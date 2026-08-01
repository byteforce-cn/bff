// ============================================================
// 公共工具函数 — 用于 frontend 所有页面
// ============================================================

/** HTML 转义，防 XSS */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/** 复制文本到剪贴板，返回是否成功 */
export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // 降级方案
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
      return true;
    } catch {
      return false;
    }
  }
}

/** 字节数 → 人类可读 (B/KB/MB) */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/** 毫秒 → 人类可读耗时 (42ms / 1.2s / 3m42s) */
export function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m${s}s`;
}

/** 秒数 → 运行时长 (00:03:42) */
export function formatElapsed(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
}

/** 统一错误消息格式化：区分网络错误、超时、服务端错误 */
export function formatError(e: unknown): string {
  if (e instanceof TypeError) {
    if (e.message.includes("Failed to fetch")) {
      return "无法连接到 BFF 服务，请检查 BFF 是否已启动";
    }
    if (e.message.includes("NetworkError")) {
      return "网络错误，请检查网络连接";
    }
  }
  if (e instanceof DOMException && e.name === "AbortError") {
    return "请求已超时或被取消";
  }
  if (e instanceof Error) return e.message;
  return String(e);
}

/** 耗时颜色类名（<100ms 绿 / 100-500ms 黄 / >500ms 红） */
export function latencyColor(ms: number): string {
  if (ms < 100) return "latency-ok";
  if (ms < 500) return "latency-warn";
  return "latency-err";
}
