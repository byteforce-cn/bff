// ============================================================
// 会话管理：查看活跃 Session 列表 + 模拟登录
// ============================================================

import { useCallback, useEffect, useRef, useState } from "react";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { TableSkeleton } from "@/components/ui/table-skeleton";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Label } from "@/components/ui/label";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { listProviders, listSessions, revokeSession } from "@/lib/api";
import type { SessionInfo } from "@/types";
import { Users, RefreshCw, Clock, Trash2, LogIn } from "lucide-react";
import { toast } from "sonner";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export default function SessionsPage() {
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [revokeId, setRevokeId] = useState<string | null>(null);

  // 模拟登录
  const [showSimulateDialog, setShowSimulateDialog] = useState(false);
  const [providers, setProviders] = useState<{ id: string; display_name?: string }[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string>("");
  const [simulating, setSimulating] = useState(false);
  const popupRef = useRef<Window | null>(null);

  const load = useCallback(async () => {
    try {
      const data = await listSessions();
      const s = data as { sessions: SessionInfo[]; count: number };
      setSessions(s.sessions || []);
    } catch (e) {
      toast.error(`加载失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  // 加载 provider 列表
  const loadProviders = useCallback(async () => {
    try {
      const data = await listProviders();
      const list = (data as { providers?: { id: string; display_name?: string }[] }).providers || (data as { id: string; display_name?: string }[]);
      setProviders(Array.isArray(list) ? list : []);
    } catch {
      toast.error("加载 Provider 列表失败");
    }
  }, []);

  useEffect(() => {
    if (showSimulateDialog) loadProviders();
  }, [showSimulateDialog, loadProviders]);

  // 执行模拟登录
  const handleSimulateLogin = () => {
    if (!selectedProvider) return;
    setSimulating(true);
    setShowSimulateDialog(false);

    const redirect = "/admin/sessions";
    // OIDC /login 在 business port (8080)，admin-ui 在 admin port (8443)，
    // 需要构造 business port 的绝对 URL，否则会命中 admin port 的 SPA fallback。
    const businessOrigin = window.location.origin.replace(
      window.location.port,
      window.location.port === "8443" ? "8080" : window.location.port
    );
    const url = `${businessOrigin}/login?provider=${encodeURIComponent(selectedProvider)}&redirect=${encodeURIComponent(redirect)}&popup=true`;

    const popup = window.open(url, "oidc-simulate", "width=500,height=700");
    popupRef.current = popup;

    if (!popup) {
      // 弹窗被拦截，降级为整页跳转
      toast.info("弹窗被拦截，将整页跳转进行登录...");
      window.location.href = `${businessOrigin}/login?provider=${encodeURIComponent(selectedProvider)}&redirect=${encodeURIComponent(redirect)}`;
      return;
    }

    // 监听 postMessage
    const onMessage = (e: MessageEvent) => {
      if (e.data === "oidc-done" && e.origin === window.location.origin) {
        cleanup();
        setSimulating(false);
        load();
        toast.success("模拟登录完成，Session 已创建");
      }
    };
    window.addEventListener("message", onMessage);

    // 监听弹窗关闭
    const timer = setInterval(() => {
      if (popup.closed) {
        cleanup();
        setSimulating(false);
        load();
        toast.info("登录窗口已关闭");
      }
    }, 500);

    const cleanup = () => {
      window.removeEventListener("message", onMessage);
      clearInterval(timer);
    };
  };

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">会话管理</h1>
          <p className="text-sm text-muted-foreground mt-1">
            当前活跃的 BFF Session 列表
            {!loading && (
              <Badge variant="secondary" className="ml-2">
                {sessions.length} 个会话
              </Badge>
            )}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            onClick={() => setShowSimulateDialog(true)}
            disabled={simulating}
          >
            <LogIn className="mr-2 h-4 w-4" />
            {simulating ? "登录中..." : "模拟登录"}
          </Button>
          <Button variant="outline" onClick={load} disabled={loading}>
            <RefreshCw className={`mr-2 h-4 w-4 ${loading ? "animate-spin" : ""}`} />
            刷新
          </Button>
        </div>
      </div>

      <Card>
        <CardContent className="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Session ID</TableHead>
                <TableHead>创建时间</TableHead>
                <TableHead>过期时间</TableHead>
                <TableHead>其他数据</TableHead>
                <TableHead className="w-16">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {loading ? (
                <TableSkeleton rows={5} cols={5} />
              ) : sessions.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={5} className="text-center text-muted-foreground py-8">
                    <Users className="mx-auto mb-2 h-8 w-8 text-muted-foreground/50" />
                    暂无活跃会话
                  </TableCell>
                </TableRow>
              ) : (
                sessions.map((s) => (
                  <TableRow key={s.id}>
                    <TableCell className="font-mono text-xs max-w-[200px] truncate" title={s.id}>
                      {s.id}
                    </TableCell>
                    <TableCell>
                      <Clock className="mr-1 inline h-3 w-3 text-muted-foreground" />
                      {s.created_at || "-"}
                    </TableCell>
                    <TableCell>{s.expires_at || "-"}</TableCell>
                    <TableCell>
                      <pre className="text-xs text-muted-foreground max-w-[300px] truncate">
                        {JSON.stringify(
                          Object.fromEntries(
                            Object.entries(s).filter(([k]) => !["id", "created_at", "expires_at"].includes(k))
                          ),
                          null,
                          2
                        )}
                      </pre>
                    </TableCell>
                    <TableCell>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <Button variant="ghost" size="icon" onClick={() => setRevokeId(s.id)}>
                            <Trash2 className="h-4 w-4 text-destructive" />
                          </Button>
                        </TooltipTrigger>
                        <TooltipContent>撤销会话</TooltipContent>
                      </Tooltip>
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <ConfirmDialog
        open={revokeId !== null}
        onOpenChange={(open) => { if (!open) setRevokeId(null); }}
        title="撤销会话"
        description={`确定要撤销会话 "${revokeId?.slice(0, 8)}…" 吗？用户将被强制登出。`}
        onConfirm={async () => {
          if (!revokeId) return;
          try {
            await revokeSession(revokeId);
            toast.success("会话已撤销");
            setRevokeId(null);
            load();
          } catch (e) {
            toast.error(`撤销失败: ${errMsg(e)}`);
          }
        }}
        variant="destructive"
        confirmText="撤销"
      />

      {/* 模拟登录 Dialog */}
      <Dialog open={showSimulateDialog} onOpenChange={setShowSimulateDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>模拟登录</DialogTitle>
            <DialogDescription>
              选择一个 OIDC Provider 发起模拟登录，完成后将生成可用于测试 API 的 Session。
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4 py-4">
            <div className="space-y-2">
              <Label>OIDC Provider</Label>
              <Select value={selectedProvider} onValueChange={setSelectedProvider}>
                <SelectTrigger>
                  <SelectValue placeholder="选择 Provider..." />
                </SelectTrigger>
                <SelectContent>
                  {providers.map((p) => (
                    <SelectItem key={p.id} value={p.id}>
                      {p.display_name || p.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setShowSimulateDialog(false)}>
              取消
            </Button>
            <Button onClick={handleSimulateLogin} disabled={!selectedProvider}>
              开始登录
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
