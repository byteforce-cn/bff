// ============================================================
// 路由管理：统一路由定义（v2）管理
// ============================================================

import { useCallback, useEffect, useMemo, useState } from "react";
import { Card, CardContent } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
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
import { MethodBadgeList } from "@/components/ui/method-badge";
import { KeyValueEditor } from "@/components/ui/key-value-editor";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { listRoutes, updateRoutes } from "@/lib/api";
import type { RouteDef } from "@/types";
import {
  Plus,
  Pencil,
  Trash2,
  Route,
  Globe,
  ShieldCheck,
  Workflow,
  Code2,
  Server,
} from "lucide-react";
import { toast } from "sonner";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const ROUTE_TYPES = ["proxy", "pipeline", "script", "static"] as const;

const EMPTY_ROUTE: RouteDef = {
  path: "",
  methods: [],
  description: "",
  auth_required: true,
  type: "proxy",
  config: { upstream: "", strip_prefix: false, circuit_breaker_threshold: 0 },
  input_mapping: { from_query: {}, from_body: {}, from_path: {}, from_header: {}, from_session: {}, from_env: {}, defaults: {} },
  output_mapping: { wrap: undefined, status_map: {}, rename: {}, pick: [] },
};

const TYPE_ICONS: Record<string, React.ReactNode> = {
  proxy: <Globe className="h-4 w-4" />,
  pipeline: <Workflow className="h-4 w-4" />,
  script: <Code2 className="h-4 w-4" />,
  static: <Server className="h-4 w-4" />,
};

const TYPE_LABELS: Record<string, string> = {
  proxy: "代理",
  pipeline: "编排",
  script: "脚本",
  static: "静态",
};

export default function RoutesPage() {
  const [routes, setRoutes] = useState<RouteDef[]>([]);
  const [loading, setLoading] = useState(true);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<RouteDef>({ ...EMPTY_ROUTE });
  const [editIndex, setEditIndex] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [showInputMapping, setShowInputMapping] = useState(false);
  const [showOutputMapping, setShowOutputMapping] = useState(false);
  const [deleteIndex, setDeleteIndex] = useState<number | null>(null);
  const [search, setSearch] = useState("");
  const [typeFilter, setTypeFilter] = useState<string>("all");

  const load = useCallback(async () => {
    try {
      const data = await listRoutes();
      setRoutes((data as { routes: RouteDef[] }).routes || []);
    } catch (e) {
      toast.error(`加载失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const openCreate = () => {
    setEditing({ ...EMPTY_ROUTE });
    setEditIndex(null);
    setShowInputMapping(false);
    setShowOutputMapping(false);
    setDialogOpen(true);
  };

  const openEdit = (index: number) => {
    const existing = routes[index];
    setEditing({
      ...JSON.parse(JSON.stringify(existing)),
      config: { upstream: "", strip_prefix: false, circuit_breaker_threshold: 0, ...existing.config },
      input_mapping: { ...existing.input_mapping },
      output_mapping: { ...existing.output_mapping },
    });
    setEditIndex(index);
    setShowInputMapping(false);
    setShowOutputMapping(false);
    setDialogOpen(true);
  };

  const handleSave = async () => {
    if (!editing.path.trim()) { toast.error("路径不能为空"); return; }
    if (editing.type === "proxy" && !editing.config.upstream?.trim()) {
      toast.error("代理类型必须填写上游地址"); return;
    }
    setSaving(true);
    try {
      const updated = editIndex !== null
        ? routes.map((r, i) => (i === editIndex ? editing : r))
        : [...routes, editing];
      await updateRoutes(updated as unknown as Record<string, unknown>[]);
      toast.success(editIndex !== null ? "路由已更新" : "路由已创建");
      setDialogOpen(false);
      load();
    } catch (e) {
      toast.error(`保存失败: ${errMsg(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (deleteIndex === null) return;
    try {
      const updated = routes.filter((_, i) => i !== deleteIndex);
      await updateRoutes(updated as unknown as Record<string, unknown>[]);
      toast.success("路由已删除");
      setDeleteIndex(null);
      load();
    } catch (e) {
      toast.error(`删除失败: ${errMsg(e)}`);
    }
  };

  const typeConfigForm = () => {
    switch (editing.type) {
      case "proxy":
        return (
          <>
            <div className="space-y-2">
              <Label htmlFor="rupstream">上游地址 *</Label>
              <Input
                id="rupstream"
                value={editing.config.upstream || ""}
                onChange={(e) => setEditing({ ...editing, config: { ...editing.config, upstream: e.target.value } })}
                placeholder="http://upstream-service"
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="rthreshold">熔断阈值</Label>
              <Input
                id="rthreshold"
                type="number"
                value={editing.config.circuit_breaker_threshold || 0}
                onChange={(e) => setEditing({ ...editing, config: { ...editing.config, circuit_breaker_threshold: parseInt(e.target.value) || 0 } })}
              />
            </div>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={editing.config.strip_prefix || false}
                onChange={(e) => setEditing({ ...editing, config: { ...editing.config, strip_prefix: e.target.checked } })}
                className="rounded"
              />
              剥离路径前缀
            </label>
          </>
        );
      case "pipeline":
        return (
          <div className="space-y-2">
            <Label htmlFor="rpipeline">Pipeline 名称</Label>
            <Input
              id="rpipeline"
              value={editing.config.pipeline || ""}
              onChange={(e) => setEditing({ ...editing, config: { ...editing.config, pipeline: e.target.value || undefined } })}
              placeholder="dashboard"
            />
          </div>
        );
      case "script":
        return (
          <div className="space-y-2">
            <Label htmlFor="rscript">脚本名称</Label>
            <Input
              id="rscript"
              value={editing.config.script || ""}
              onChange={(e) => setEditing({ ...editing, config: { ...editing.config, script: e.target.value || undefined } })}
              placeholder="transform.rhai"
            />
          </div>
        );
      case "static":
        return (
          <>
            <div className="space-y-2">
              <Label htmlFor="rstatus">HTTP 状态码</Label>
              <Input
                id="rstatus"
                type="number"
                value={editing.config.status || 200}
                onChange={(e) => setEditing({ ...editing, config: { ...editing.config, status: parseInt(e.target.value) || 200 } })}
              />
            </div>
          </>
        );
      default:
        return null;
    }
  };

  // 筛选与搜索
  const filteredRoutes = useMemo(() => {
    return routes.filter((r) => {
      if (typeFilter !== "all" && r.type !== typeFilter) return false;
      if (!search.trim()) return true;
      const q = search.toLowerCase();
      return (
        r.path.toLowerCase().includes(q) ||
        (r.description || "").toLowerCase().includes(q) ||
        (r.config.upstream || "").toLowerCase().includes(q)
      );
    });
  }, [routes, search, typeFilter]);

  const getTargetDisplay = (r: RouteDef) => {
    switch (r.type) {
      case "proxy":
        return r.config.upstream || "—";
      case "pipeline":
        return r.config.pipeline || "—";
      case "script":
        return r.config.script || "—";
      case "static":
        return r.config.status?.toString() || "200";
      default:
        return "—";
    }
  };

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">路由管理</h1>
          <p className="text-sm text-muted-foreground mt-1">统一管理代理、编排、脚本、静态路由</p>
        </div>
        <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
          <DialogTrigger asChild>
            <Button onClick={openCreate}>
              <Plus className="mr-2 h-4 w-4" /> 添加路由
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-xl max-h-[80vh] overflow-y-auto">
            <DialogHeader>
              <DialogTitle>{editIndex !== null ? "编辑路由" : "新增路由"}</DialogTitle>
            </DialogHeader>
            <div className="grid gap-4 py-4">
              {/* 基本信息 */}
              <div className="grid grid-cols-2 gap-4">
                <div className="space-y-2">
                  <Label htmlFor="rpath">路径 *</Label>
                  <Input
                    id="rpath"
                    value={editing.path}
                    onChange={(e) => setEditing({ ...editing, path: e.target.value })}
                    placeholder="/api/users"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="rtype">类型</Label>
                  <select
                    id="rtype"
                    className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                    value={editing.type}
                    onChange={(e) => {
                      const newType = e.target.value as RouteDef["type"];
                      if (newType !== editing.type && editing.config && Object.keys(editing.config).some(k => editing.config[k as keyof typeof editing.config])) {
                        if (!confirm("切换路由类型将清空当前类型配置，是否继续？")) return;
                      }
                      setEditing({
                        ...editing,
                        type: newType,
                        config: { ...EMPTY_ROUTE.config },
                      });
                    }}
                  >
                    {ROUTE_TYPES.map((t) => (
                      <option key={t} value={t}>{TYPE_LABELS[t]} ({t})</option>
                    ))}
                  </select>
                </div>
              </div>

              <div className="space-y-2">
                <Label htmlFor="rdesc">描述</Label>
                <Input
                  id="rdesc"
                  value={editing.description}
                  onChange={(e) => setEditing({ ...editing, description: e.target.value })}
                  placeholder="路由用途说明"
                />
              </div>

              <div className="flex items-center gap-6">
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={editing.auth_required}
                    onChange={(e) => setEditing({ ...editing, auth_required: e.target.checked })}
                    className="rounded"
                  />
                  需要认证
                </label>
              </div>

              {/* 类型专属配置 */}
              <div className="border-t pt-4">
                <h4 className="text-sm font-medium mb-3">类型配置</h4>
                {typeConfigForm()}
              </div>

              {/* 输入映射（KeyValueEditor） */}
              <div className="border-t pt-4">
                <button
                  type="button"
                  className="flex items-center gap-2 text-sm font-medium w-full text-left"
                  onClick={() => setShowInputMapping(!showInputMapping)}
                >
                  <span className={`transform transition-transform ${showInputMapping ? "rotate-90" : ""}`}>▶</span>
                  输入映射（高级）
                </button>
                {showInputMapping && (
                  <div className="mt-3 space-y-3">
                    <KeyValueEditor
                      title="from_query"
                      entries={editing.input_mapping.from_query}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, from_query: v } })}
                    />
                    <KeyValueEditor
                      title="from_body"
                      entries={editing.input_mapping.from_body}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, from_body: v } })}
                    />
                    <KeyValueEditor
                      title="from_path"
                      entries={editing.input_mapping.from_path}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, from_path: v } })}
                    />
                    <KeyValueEditor
                      title="from_header"
                      entries={editing.input_mapping.from_header}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, from_header: v } })}
                    />
                    <KeyValueEditor
                      title="from_session"
                      entries={editing.input_mapping.from_session}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, from_session: v } })}
                      keyPlaceholder="目标变量"
                      valuePlaceholder="session key"
                    />
                    <KeyValueEditor
                      title="from_env"
                      entries={editing.input_mapping.from_env}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, from_env: v } })}
                      keyPlaceholder="目标变量"
                      valuePlaceholder="环境变量名"
                    />
                    <KeyValueEditor
                      title="defaults"
                      entries={Object.fromEntries(
                        Object.entries(editing.input_mapping.defaults).map(([k, v]) => [k, String(v)])
                      )}
                      onChange={(v) => setEditing({ ...editing, input_mapping: { ...editing.input_mapping, defaults: v as unknown as Record<string, unknown> } })}
                    />
                  </div>
                )}
              </div>

              {/* 输出映射 */}
              <div className="border-t pt-4">
                <button
                  type="button"
                  className="flex items-center gap-2 text-sm font-medium w-full text-left"
                  onClick={() => setShowOutputMapping(!showOutputMapping)}
                >
                  <span className={`transform transition-transform ${showOutputMapping ? "rotate-90" : ""}`}>▶</span>
                  输出映射（高级）
                </button>
                {showOutputMapping && (
                  <div className="mt-3 space-y-3">
                    <div className="space-y-2">
                      <Label className="text-xs">wrap（包裹键名）</Label>
                      <Input
                        value={editing.output_mapping.wrap || ""}
                        onChange={(e) => setEditing({ ...editing, output_mapping: { ...editing.output_mapping, wrap: e.target.value || undefined } })}
                        placeholder="data"
                      />
                    </div>
                    <KeyValueEditor
                      title="rename"
                      entries={editing.output_mapping.rename}
                      onChange={(v) => setEditing({ ...editing, output_mapping: { ...editing.output_mapping, rename: v } })}
                      keyPlaceholder="新名称"
                      valuePlaceholder="旧名称"
                    />
                  </div>
                )}
              </div>
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

      {/* 搜索与筛选 */}
      <div className="flex gap-2 flex-wrap">
        <Input
          placeholder="搜索路径、描述或上游地址…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="max-w-sm"
        />
        <div className="flex gap-1 flex-wrap">
          {["all", ...ROUTE_TYPES].map((t) => (
            <Badge
              key={t}
              variant={typeFilter === t ? "default" : "secondary"}
              className="cursor-pointer"
              onClick={() => setTypeFilter(t)}
            >
              {t === "all" ? "全部" : TYPE_LABELS[t]}
            </Badge>
          ))}
        </div>
      </div>

      <Card>
        <CardContent className="p-0">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>路径</TableHead>
                <TableHead>Methods</TableHead>
                <TableHead>类型</TableHead>
                <TableHead>目标地址 / 名称</TableHead>
                <TableHead>认证</TableHead>
                <TableHead className="w-24">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {loading ? (
                <TableSkeleton rows={5} cols={6} />
              ) : filteredRoutes.length === 0 ? (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-muted-foreground py-8">
                    <Route className="mx-auto mb-2 h-8 w-8 text-muted-foreground/50" />
                    {routes.length === 0 ? "暂无路由，点击「添加路由」开始配置" : "没有匹配的路由"}
                  </TableCell>
                </TableRow>
              ) : (
                filteredRoutes.map((r, i) => {
                  const realIndex = routes.indexOf(r);
                  return (
                    <TableRow key={realIndex}>
                      <TableCell className="font-medium">
                        <Route className="mr-2 inline h-4 w-4 text-muted-foreground" />
                        {r.path}
                      </TableCell>
                      <TableCell>
                        <MethodBadgeList methods={r.methods} />
                      </TableCell>
                      <TableCell>
                        <span className="inline-flex items-center gap-1 text-xs">
                          {TYPE_ICONS[r.type]}
                          {TYPE_LABELS[r.type] || r.type}
                        </span>
                      </TableCell>
                      <TableCell className="text-muted-foreground text-sm max-w-[200px] truncate" title={getTargetDisplay(r)}>
                        {getTargetDisplay(r)}
                      </TableCell>
                      <TableCell>
                        {r.auth_required ? (
                          <ShieldCheck className="h-4 w-4 text-green-500" />
                        ) : (
                          <span className="text-muted-foreground text-xs">—</span>
                        )}
                      </TableCell>
                      <TableCell>
                        <div className="flex gap-1">
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button variant="ghost" size="icon" onClick={() => openEdit(realIndex)}>
                                <Pencil className="h-4 w-4" />
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>编辑</TooltipContent>
                          </Tooltip>
                          <Tooltip>
                            <TooltipTrigger asChild>
                              <Button variant="ghost" size="icon" onClick={() => setDeleteIndex(realIndex)}>
                                <Trash2 className="h-4 w-4 text-destructive" />
                              </Button>
                            </TooltipTrigger>
                            <TooltipContent>删除</TooltipContent>
                          </Tooltip>
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      {/* 删除确认 */}
      <ConfirmDialog
        open={deleteIndex !== null}
        onOpenChange={(open) => { if (!open) setDeleteIndex(null); }}
        title="删除路由"
        description={deleteIndex !== null ? `确定要删除路由 "${routes[deleteIndex]?.path}" 吗？此操作不可撤销。` : ""}
        onConfirm={handleDelete}
        variant="destructive"
      />
    </div>
  );
}
