// ============================================================
// 脚本管理：编辑、Eval 调试
// ============================================================

import { useCallback, useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
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
import { listScripts, updateScript, evalScript } from "@/lib/api";
import { FileCode, Play, Pencil, RefreshCw, Terminal, Plus, Trash2 } from "lucide-react";
import { toast } from "sonner";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export default function ScriptsPage() {
  const [scripts, setScripts] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [editName, setEditName] = useState("");
  const [editContent, setEditContent] = useState("");
  const [dialogOpen, setDialogOpen] = useState(false);

  // Eval
  const [evalName, setEvalName] = useState("");
  const [evalInputs, setEvalInputs] = useState("{}");
  const [evalSession, setEvalSession] = useState("");
  const [evalEnv, setEvalEnv] = useState("");
  const [evalResult, setEvalResult] = useState("");
  const [evalRunning, setEvalRunning] = useState(false);
  const [newScriptName, setNewScriptName] = useState("");
  const [newScriptOpen, setNewScriptOpen] = useState(false);

  const load = useCallback(async () => {
    try {
      const data = await listScripts();
      setScripts((data as { scripts: Record<string, string> }).scripts || {});
    } catch (e) {
      toast.error(`加载失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const openEdit = (name: string, content: string) => {
    setEditName(name);
    setEditContent(content);
    setDialogOpen(true);
  };

  const handleSave = async () => {
    if (!editName.trim()) { toast.error("名称不能为空"); return; }
    try {
      await updateScript(editName, editContent);
      toast.success(`脚本 "${editName}" 已保存`);
      setDialogOpen(false);
      load();
    } catch (e) {
      toast.error(`保存失败: ${errMsg(e)}`);
    }
  };

  const handleEval = async () => {
    if (!evalName.trim()) { toast.error("请选择脚本"); return; }
    setEvalRunning(true);
    setEvalResult("执行中…");
    try {
      let inputs: Record<string, unknown> = {};
      try {
        inputs = JSON.parse(evalInputs);
      } catch {
        toast.error("Inputs JSON 格式错误");
        setEvalRunning(false);
        return;
      }
      let session: Record<string, unknown> | undefined;
      if (evalSession.trim()) {
        try { session = JSON.parse(evalSession); } catch {
          toast.error("Session JSON 格式错误");
          setEvalRunning(false);
          return;
        }
      }
      let env: Record<string, unknown> | undefined;
      if (evalEnv.trim()) {
        try { env = JSON.parse(evalEnv); } catch {
          toast.error("Env JSON 格式错误");
          setEvalRunning(false);
          return;
        }
      }
      const result = await evalScript(evalName, editContent || undefined as unknown as string, inputs, session, env);
      setEvalResult(JSON.stringify(result, null, 2));
    } catch (e) {
      setEvalResult(`执行失败: ${errMsg(e)}`);
    } finally {
      setEvalRunning(false);
    }
  };

  const scriptNames = Object.keys(scripts);

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">脚本管理</h1>
          <p className="text-sm text-muted-foreground mt-1">管理 Rhai 脚本并在线调试</p>
        </div>
        <div className="flex gap-2">
          <Dialog open={newScriptOpen} onOpenChange={setNewScriptOpen}>
            <DialogTrigger asChild>
              <Button variant="default">
                <Plus className="mr-2 h-4 w-4" /> 新建脚本
              </Button>
            </DialogTrigger>
            <DialogContent className="max-w-md">
              <DialogHeader>
                <DialogTitle>新建脚本</DialogTitle>
              </DialogHeader>
              <div className="space-y-4 py-4">
                <div className="space-y-2">
                  <Label>脚本名称</Label>
                  <Input
                    value={newScriptName}
                    onChange={(e) => setNewScriptName(e.target.value)}
                    placeholder="例如 transform.rhai"
                  />
                </div>
              </div>
              <div className="flex justify-end gap-2">
                <Button variant="outline" onClick={() => setNewScriptOpen(false)}>取消</Button>
                <Button onClick={async () => {
                  if (!newScriptName.trim()) { toast.error("名称不能为空"); return; }
                  try {
                    await updateScript(newScriptName.trim(), "// Rhai script");
                    toast.success(`脚本 "${newScriptName}" 已创建`);
                    setNewScriptOpen(false);
                    setNewScriptName("");
                    load();
                  } catch (e) {
                    toast.error(`创建失败: ${errMsg(e)}`);
                  }
                }}>创建</Button>
              </div>
            </DialogContent>
          </Dialog>
          <Button variant="outline" onClick={load} disabled={loading}>
            <RefreshCw className={`mr-2 h-4 w-4 ${loading ? "animate-spin" : ""}`} />
            刷新
          </Button>
        </div>
      </div>

      <Tabs defaultValue="list" className="w-full">
        <TabsList>
          <TabsTrigger value="list">脚本列表</TabsTrigger>
          <TabsTrigger value="eval">在线调试</TabsTrigger>
        </TabsList>

        <TabsContent value="list">
          <Card>
            <CardContent className="p-0">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>脚本名称</TableHead>
                    <TableHead>大小</TableHead>
                    <TableHead>预览</TableHead>
                    <TableHead className="w-24">操作</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {loading ? (
                    <TableSkeleton rows={5} cols={4} />
                  ) : scriptNames.length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={4} className="text-center text-muted-foreground py-8">
                        暂无脚本。脚本可从 config/scripts/ 目录加载，或通过 API 上传。
                      </TableCell>
                    </TableRow>
                  ) : (
                    scriptNames.map((name) => (
                      <TableRow key={name}>
                        <TableCell className="font-medium">
                          <FileCode className="mr-2 inline h-4 w-4 text-muted-foreground" />
                          {name}
                        </TableCell>
                        <TableCell>
                          <Badge variant="secondary">{scripts[name].length} B</Badge>
                        </TableCell>
                        <TableCell className="max-w-[300px] truncate font-mono text-xs">
                          {scripts[name].slice(0, 80)}
                          {scripts[name].length > 80 ? "…" : ""}
                        </TableCell>
                        <TableCell>
                          <div className="flex gap-1">
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => openEdit(name, scripts[name])}
                            >
                              <Pencil className="h-4 w-4" />
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))
                  )}
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="eval" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle className="text-base">
                <Terminal className="mr-2 inline h-4 w-4" />
                脚本调试器
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-2">
                <Label>选择脚本</Label>
                <select
                  className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm"
                  value={evalName}
                  onChange={(e) => {
                    setEvalName(e.target.value);
                    if (scripts[e.target.value]) setEditContent(scripts[e.target.value]);
                  }}
                >
                  <option value="">-- 选择脚本 --</option>
                  {scriptNames.map((n) => (
                    <option key={n} value={n}>{n}</option>
                  ))}
                </select>
              </div>
              <div className="space-y-2">
                <Label>脚本内容（可编辑）</Label>
                <Textarea
                  className="min-h-[200px] font-mono text-sm"
                  value={editContent}
                  onChange={(e) => setEditContent(e.target.value)}
                  placeholder="选择脚本后自动填充…"
                />
              </div>
              <div className="space-y-2">
                <Label>输入参数（JSON）</Label>
                <Textarea
                  className="min-h-[80px] font-mono text-sm"
                  value={evalInputs}
                  onChange={(e) => setEvalInputs(e.target.value)}
                  placeholder='{"key": "value"}'
                />
              </div>
              <div className="space-y-2">
                <Label>模拟 Session（JSON，可选）</Label>
                <Textarea
                  className="min-h-[60px] font-mono text-sm"
                  value={evalSession}
                  onChange={(e) => setEvalSession(e.target.value)}
                  placeholder='{"sub": "user-123", "access_token": "..."}'
                />
              </div>
              <div className="space-y-2">
                <Label>模拟环境变量（JSON，可选）</Label>
                <Textarea
                  className="min-h-[60px] font-mono text-sm"
                  value={evalEnv}
                  onChange={(e) => setEvalEnv(e.target.value)}
                  placeholder='{"API_KEY": "secret"}'
                />
              </div>
              <div className="flex gap-2">
                <Button onClick={handleEval} disabled={evalRunning}>
                  <Play className="mr-2 h-4 w-4" />
                  {evalRunning ? "执行中…" : "执行"}
                </Button>
                {evalName && editContent && (
                  <Button
                    variant="outline"
                    onClick={async () => {
                      try {
                        await updateScript(evalName, editContent);
                        toast.success(`脚本 "${evalName}" 已保存`);
                        load();
                      } catch (e) {
                        toast.error(`保存失败: ${errMsg(e)}`);
                      }
                    }}
                  >
                    保存到服务端
                  </Button>
                )}
              </div>
              {evalResult && (
                <div className="space-y-1">
                  <Label>执行结果</Label>
                  <pre className="rounded-lg bg-muted p-4 text-sm overflow-auto max-h-[300px]">
                    {evalResult}
                  </pre>
                </div>
              )}
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>

      {/* 编辑脚本 Dialog */}
      <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>编辑脚本</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4 py-4">
            <div className="space-y-2">
              <Label>脚本名称</Label>
              <Input value={editName} onChange={(e) => setEditName(e.target.value)} />
            </div>
            <div className="space-y-2">
              <Label>脚本内容 (Rhai)</Label>
              <Textarea
                className="min-h-[350px] font-mono text-sm"
                value={editContent}
                onChange={(e) => setEditContent(e.target.value)}
              />
            </div>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => setDialogOpen(false)}>取消</Button>
            <Button onClick={handleSave}>保存</Button>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
