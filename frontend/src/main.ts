// ============================================================
// BFF 测试 SPA — 极简 hash 路由，零框架依赖
// ============================================================

import { renderHome } from "./pages/home";
import { renderLogin } from "./pages/login";
import { renderDashboard } from "./pages/dashboard";
import { renderProxy } from "./pages/proxy";

type PageModule = { render: (el: HTMLElement) => void };

const routes: Record<string, PageModule> = {
  "/": { render: renderHome },
  "/login": { render: renderLogin },
  "/dashboard": { render: renderDashboard },
  "/proxy": { render: renderProxy },
};

const viewEl = document.getElementById("view")!;
const navLinks = document.querySelectorAll("#nav a");

function matchRoute(): PageModule {
  const hash = window.location.hash.slice(1) || "/";
  // 支持 /dashboard/settings 等子路径匹配到 /dashboard
  for (const [prefix, mod] of Object.entries(routes)) {
    if (hash === prefix || (prefix !== "/" && hash.startsWith(prefix))) {
      return mod;
    }
  }
  return { render: renderHome };
}

function highlightNav() {
  const hash = window.location.hash.slice(1) || "/";
  navLinks.forEach((a) => {
    const href = a.getAttribute("href")?.slice(1) || "/";
    if (hash === href || (href !== "/" && hash.startsWith(href))) {
      a.classList.add("active");
    } else {
      a.classList.remove("active");
    }
  });
}

function router() {
  const mod = matchRoute();
  viewEl.innerHTML = "";
  mod.render(viewEl);
  highlightNav();
}

window.addEventListener("hashchange", router);
window.addEventListener("DOMContentLoaded", router);
