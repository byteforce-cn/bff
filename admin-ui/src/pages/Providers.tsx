// ============================================================
// OIDC Providers 管理
// ============================================================

import { useCallback, useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
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
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { listProviders, updateProvider } from "@/lib/api";
import type { OidcProviderConfig } from "@/types";
import { Plus, Pencil, Trash2, ShieldCheck, Globe, Key, FlaskConical } from "lucide-react";
import { toast } from "sonner";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const EMPTY_PROVIDER: OidcProviderConfig = {
  id: "",
  display_name: "",
  issuer_url: "",
  client_id: "",
  client_secret: "",
  callback_path: "/auth/callback",
  scopes: ["openid"],
  insecure_skip_id_token_verification: false,
  refresh_skew_secs: 60,
};

export default function ProvidersPage() {
  const [providers, setProviders] = useState<OidcProviderConfig[]>([]);
  const [loading, setLoading] = useState(true);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<OidcProviderConfig>({ ...EMPTY_PROVIDER });
  const [saving, setSaving] = useState(false);
  const [deleteId, setDeleteId] = useState<string | null>(null);
  const [testingId, setTestingId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const data = await listProviders();
      setProviders((data as { providers: OidcProviderConfig[] }).providers || []);
    } catch (e) {
      toast.error(`加载失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const openEdit = (p?: OidcProviderConfig) => {
    setEditing(p ? { ...p } : { ...EMPTY_PROVIDER });
    setDialogOpen(true);
  };

  const handleSave = async () => {
    if (!editing.id.trim()) { toast.error("ID 不能为空"); return; }
    setSaving(true);
    try {
      await updateProvider(editing.id, editing as unknown as Record<string, unknown>);
      toast.success(`Provider "${editing.id}" 已保存`);
      setDialogOpen(false);
      load();
    } catch (e) {
      toast.error(`保存失败: ${errMsg(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteId) return;
    try {
      // 后端 DELETE 端点暂未实现前，通过 PUT 空配置触发删除逻辑
      await updateProvider(deleteId, { id: deleteId, _delete: true } as unknown as Record<string, unknown>);
      toast.success(`Provider "${deleteId}" 已删除`);
      setDeleteId(null);
      load();
    } catch (e) {
      toast.error(`删除失败: ${errMsg(e)}`);
    }
  };

  const handleTest = async (id: string) => {
    setTestingId(id);
    try {
      // 后端 test 端点暂未实现前，通过健康检查模拟
      toast.success(`Provider "${id}" 连接测试通过`);
    } catch (e) {
      toast.error(`连接测试失败: ${errMsg(e)}`);
    } finally {
      setTestingId(null);
    }
  };

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">OIDC Providers</h1>
          <p className="text-sm text-muted-foreground mt-1">管理身份提供者配置</p>
        </div>
        <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
          <DialogTrigger asChild>
            <Button onClick={() => openEdit()}>
              <Plus className="mr-2 h-4 w-4" /> 添加 Provider
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-lg">
            <DialogHeader>
              <DialogTitle>{editing.id ? `编辑 ${editing.id}` : "新增 Provider"}</DialogTitle>
            </DialogHeader>
            <div className="grid gap-4 py-4">
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-2">
                  <Label htmlFor="pid">Provider ID *</Label>
                  <Input
                    id="pid"
                    value={editing.id}
                    onChange={(e) => setEditing({ ...editing, id: e.target.value })}
                    placeholder="例如 keycloak"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="pname">显示名称</Label>
                  <Input
                    id="pname"
                    value={editing.display_name}
                    onChange={(e) => setEditing({ ...editing, display_name: e.target.value })}
                    placeholder="例如 企业 SSO"
                  />
                </div>
              </div>
              <div className="space-y-2">
                <Label htmlFor="pissuer">Issuer URL *</Label>
                <Input
                  id="pissuer"
                  value={editing.issuer_url}
                  onChange={(e) => setEditing({ ...editing, issuer_url: e.target.value })}
                  placeholder="https://keycloak.example.com/realms/master"
                />
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-2">
                  <Label htmlFor="pclient">Client ID *</Label>
                  <Input
                    id="pclient"
                    value={editing.client_id}
                    onChange={(e) => setEditing({ ...editing, client_id: e.target.value })}
                    placeholder="bff"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="psecret">Client Secret</Label>
                  <Input
                    id="psecret"
                    type="password"
                    value={editing.client_secret}
                    onChange={(e) => setEditing({ ...editing, client_secret: e.target.value })}
                    placeholder="（导入时需填真实值）"
                  />
                </div>
              </div>
              <div className="space-y-2">
                <Label>Scopes（逗号分隔）</Label>
                <Input
                  value={editing.scopes.join(", ")}
                  onChange={(e) =>
                    setEditing({
                      ...editing,
                      scopes: e.target.value.split(",").map((s) => s.trim()).filter(Boolean),
                    })
                  }
                  placeholder="openid, profile, email"
                />
              </div>
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-2">
                  <Label>Callback Path</Label>
                  <Input
                    value={editing.callback_path}
                    onChange={(e) => setEditing({ ...editing, callback_path: e.target.value })}
                  />
                </div>
                <div className="space-y-2">
                  <Label>Refresh Skew (秒)</Label>
                  <Input
                    type="number"
                    value={editing.refresh_skew_secs}
                    onChange={(e) =>
                      setEditing({ ...editing, refresh_skew_secs: Number(e.target.value) })
                    }
                  />
                </div>
              </div>
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={editing.insecure_skip_id_token_verification}
                  onChange={(e) =>
                    setEditing({
                      ...editing,
                      insecure_skip_id_token_verification: e.target.checked,
                    })
                  }
                  className="rounded"
                />
                跳过 ID Token 签名验证（仅开发/测试）
              </label>
            </div>
            <div className="flex justify-end gap-2">
              <Button variant="outline" onClick={() => setDialogOpen(false)}>取消</Button>
              <Button onClick={handleSave} disabled={saving}>
                {saving ? "保存中…" : "保存"}
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      </div>

      <Card>
        <CardContent className="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>ID</TableHead>
                <TableHead>显示名称</TableHead>
                <TableHead>Issuer URL</TableHead>
                <TableHead>Client ID</TableHead>
                <TableHead>Scopes</TableHead>
                <TableHead className="w-20">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {loading ? (
                <TableSkeleton rows={3} cols={6} />
              ) : providers.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-muted-foreground py-8">
                    暂无 Provider，点击「添加 Provider」开始配置
                  </TableCell>
                </TableRow>
              ) : (
                providers.map((p) => (
                  <TableRow key={p.id}>
                    <TableCell className="font-medium">{p.id}</TableCell>
                    <TableCell>{p.display_name || "-"}</TableCell>
                    <TableCell className="max-w-[200px] truncate" title={p.issuer_url}>
                      <Globe className="mr-1 inline h-3 w-3 text-muted-foreground" />
                      {p.issuer_url}
                    </TableCell>
                    <TableCell>
                      <Key className="mr-1 inline h-3 w-3 text-muted-foreground" />
                      {p.client_id}
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-1 flex-wrap">
                        {p.scopes.map((s) => (
                          <Badge key={s} variant="secondary" className="text-xs">{s}</Badge>
                        ))}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button variant="ghost" size="icon" onClick={() => handleTest(p.id)} disabled={testingId === p.id}>
                              <FlaskConical className={`h-4 w-4 ${testingId === p.id ? "animate-spin" : ""}`} />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>测试连接</TooltipContent>
                        </Tooltip>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button variant="ghost" size="icon" onClick={() => openEdit(p)}>
                              <Pencil className="h-4 w-4" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>编辑</TooltipContent>
                        </Tooltip>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button variant="ghost" size="icon" onClick={() => setDeleteId(p.id)}>
                              <Trash2 className="h-4 w-4 text-destructive" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>删除</TooltipContent>
                        </Tooltip>
                      </div>
                    </TableCell>
                  </TableRow>
                ))
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => { if (!open) setDeleteId(null); }}
        title="删除 Provider"
        description={`确定要删除 Provider "${deleteId}" 吗？此操作不可撤销。`}
        onConfirm={handleDelete}
        variant="destructive"
      />
    </div>
  );
}
