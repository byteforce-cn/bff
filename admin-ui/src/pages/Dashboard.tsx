// ============================================================
// 仪表盘：系统概览
// ============================================================

import { useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { health, listSessions, listProviders, listPipelines } from "@/lib/api";
import {
  Activity,
  Users,
  Puzzle,
  ShieldCheck,
  Server,
  ArrowRight,
} from "lucide-react";
import { Link } from "react-router-dom";

export default function DashboardPage() {
  const [stats, setStats] = useState({
    health: false,
    sessions: 0,
    providers: 0,
    pipelines: 0,
    loading: true,
    errors: {} as Record<string, string>,
  });

  useEffect(() => {
    Promise.allSettled([
      health(),
      listSessions(),
      listProviders(),
      listPipelines(),
    ])
      .then(([h, s, p, pl]) => {
        const hOk = h.status === "fulfilled";
        const sData = s.status === "fulfilled" ? (s.value as { sessions?: unknown[]; count?: number }) : null;
        const pData = p.status === "fulfilled" ? (p.value as { providers?: unknown[] }) : null;
        const plData = pl.status === "fulfilled" ? (pl.value as { pipelines?: Record<string, unknown> }) : null;

        const errors: Record<string, string> = {};
        if (s.status === "rejected") errors.sessions = (s.reason as Error)?.message || "加载失败";
        if (p.status === "rejected") errors.providers = (p.reason as Error)?.message || "加载失败";
        if (pl.status === "rejected") errors.pipelines = (pl.reason as Error)?.message || "加载失败";

        setStats({
          health: hOk,
          sessions: sData?.count ?? sData?.sessions?.length ?? 0,
          providers: pData?.providers?.length ?? 0,
          pipelines: plData?.pipelines ? Object.keys(plData.pipelines).length : 0,
          loading: false,
          errors,
        });
      })
      .catch(() =>
        setStats((s) => ({ ...s, loading: false }))
      );
  }, []);

  const cards = [
    {
      title: "服务状态",
      value: stats.health ? "运行中" : "异常",
      icon: Server,
      color: stats.health ? "text-green-600" : "text-red-600",
      badge: stats.health ? "success" as const : "destructive" as const,
      desc: "BFF 管理端口健康检查",
    },
    {
      title: "活跃会话",
      value: stats.loading ? "…" : String(stats.sessions),
      icon: Activity,
      color: "text-blue-600",
      badge: "secondary" as const,
      desc: "当前活跃的 BFF 会话数",
      link: "/sessions",
      errorKey: "sessions",
    },
    {
      title: "OIDC Providers",
      value: stats.loading ? "…" : String(stats.providers),
      icon: ShieldCheck,
      color: "text-purple-600",
      badge: "secondary" as const,
      desc: "已配置的身份提供者",
      link: "/providers",
      errorKey: "providers",
    },
    {
      title: "Pipelines",
      value: stats.loading ? "…" : String(stats.pipelines),
      icon: Puzzle,
      color: "text-orange-600",
      badge: "secondary" as const,
      desc: "已定义的编排流程",
      link: "/pipelines",
      errorKey: "pipelines",
    },
  ];

  return (
    <div className="space-y-6 p-6">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">系统概览</h1>
        <p className="text-sm text-muted-foreground mt-1">
          BFF 中间件实时状态一览
        </p>
      </div>

      {Object.keys(stats.errors).length > 0 && (
        <Card className="border-destructive">
          <CardContent className="p-4 text-sm text-destructive space-y-1">
            {Object.entries(stats.errors).map(([k, v]) => (
              <p key={k}>{k} 加载失败：{v}</p>
            ))}
          </CardContent>
        </Card>
      )}

      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        {cards.map((card) => (
          <Card key={card.title}>
            <CardHeader className="flex flex-row items-center justify-between pb-2">
              <CardTitle className="text-sm font-medium text-muted-foreground">
                {card.title}
              </CardTitle>
              <card.icon className={`h-4 w-4 ${card.color}`} />
            </CardHeader>
            <CardContent>
              <div className="flex items-center justify-between">
                <div className="text-2xl font-bold">{card.value}</div>
                <Badge variant={card.badge}>
                  {card.badge === "success" ? "正常" : stats.loading ? "加载中" : card.value}
                </Badge>
              </div>
              <p className="mt-1 text-xs text-muted-foreground">
                {card.desc}
              </p>
              {"link" in card && (
                <Link
                  to={card.link!}
                  className="mt-2 inline-flex items-center gap-1 text-xs text-primary hover:underline"
                >
                  查看详情 <ArrowRight className="h-3 w-3" />
                </Link>
              )}
            </CardContent>
          </Card>
        ))}
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-lg">快速操作</CardTitle>
        </CardHeader>
        <CardContent>
          <div className="grid gap-2 sm:grid-cols-3">
            <Link
              to="/config"
              className="flex items-center gap-3 rounded-lg border p-4 transition-colors hover:bg-muted"
            >
              <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-primary/10">
                <Server className="h-4 w-4 text-primary" />
              </div>
              <div>
                <p className="text-sm font-medium">配置导入/导出</p>
                <p className="text-xs text-muted-foreground">查看与修改 BFF 配置</p>
              </div>
            </Link>
            <Link
              to="/providers"
              className="flex items-center gap-3 rounded-lg border p-4 transition-colors hover:bg-muted"
            >
              <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-purple-50">
                <Users className="h-4 w-4 text-purple-600" />
              </div>
              <div>
                <p className="text-sm font-medium">管理 Providers</p>
                <p className="text-xs text-muted-foreground">OIDC 身份提供者配置</p>
              </div>
            </Link>
            <Link
              to="/pipelines"
              className="flex items-center gap-3 rounded-lg border p-4 transition-colors hover:bg-muted"
            >
              <div className="flex h-9 w-9 items-center justify-center rounded-lg bg-orange-50">
                <Puzzle className="h-4 w-4 text-orange-600" />
              </div>
              <div>
                <p className="text-sm font-medium">编辑 Pipelines</p>
                <p className="text-xs text-muted-foreground">编排流程定义</p>
              </div>
            </Link>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
