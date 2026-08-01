// ============================================================
// Pipelines 管理：列表、创建、删除
// ============================================================

import { useCallback, useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
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
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { listPipelines, createPipeline, deletePipeline, testPipeline } from "@/lib/api";
import type { PipelineDef, StepDef } from "@/types";
import { Plus, Trash2, Pencil, GitFork, Clock, ArrowRight, Play, Loader2, CheckCircle, XCircle, AlertTriangle } from "lucide-react";
import { toast } from "sonner";
import { cn } from "@/lib/utils";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const EXAMPLE_YAML = `strategy:
  timeout: 10s
  error_handling: fail_fast
steps:
  - id: fetch_data
    type: http_request
    config:
      url: "http://example.com/api/data"
      method: GET
  - id: transform
    type: script
    depends_on: [fetch_data]
    config:
      script: |
        let data = inputs["fetch_data"].body;
        #{ result: data }`;

export default function PipelinesPage() {
  const [pipelines, setPipelines] = useState<Record<string, PipelineDef>>({});
  const [loading, setLoading] = useState(true);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [newName, setNewName] = useState("");
  const [newYaml, setNewYaml] = useState(EXAMPLE_YAML);
  const [saving, setSaving] = useState(false);
  const [editingName, setEditingName] = useState<string | null>(null);
  const [yamlError, setYamlError] = useState("");
  const [deleteName, setDeleteName] = useState<string | null>(null);

  // Test state
  const [testDialogOpen, setTestDialogOpen] = useState(false);
  const [testName, setTestName] = useState("");
  const [testParams, setTestParams] = useState("{}");
  const [testSession, setTestSession] = useState("");
  const [testEnv, setTestEnv] = useState("");
  const [testDryRun, setTestDryRun] = useState(false);
  const [testTimeout, setTestTimeout] = useState("");
  const [testResult, setTestResult] = useState<Record<string, unknown> | null>(null);
  const [testRunning, setTestRunning] = useState(false);

  const load = useCallback(async () => {
    try {
      const data = await listPipelines();
      setPipelines((data as { pipelines: Record<string, PipelineDef> }).pipelines || {});
    } catch (e) {
      toast.error(`加载失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const validateYaml = (yaml: string): string => {
    // 基础 YAML 语法校验（不用外部库，做关键结构检查）
    if (!yaml.trim()) return "YAML 内容不能为空";
    if (!yaml.includes("strategy:")) return "缺少 strategy 定义";
    if (!yaml.includes("steps:")) return "缺少 steps 定义";
    // 检查基本缩进
    const lines = yaml.split("\n");
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (line.match(/^\t/)) return `第 ${i + 1} 行：YAML 不允许使用 Tab 缩进，请用空格`;
    }
    return "";
  };

  const openCreate = () => {
    setEditingName(null);
    setNewName("");
    setNewYaml(EXAMPLE_YAML);
    setYamlError("");
    setDialogOpen(true);
  };

  const openEdit = (name: string) => {
    setEditingName(name);
    setNewName(name);
    // 将 PipelineDef 序列化为 YAML
    const def = pipelines[name];
    if (def) {
      const yaml = `strategy:
  timeout: ${def.strategy?.timeout || "10s"}
  error_handling: ${def.strategy?.error_handling || "fail_fast"}
steps:
${(def.steps || []).map((s: StepDef) => `  - id: ${s.id}
    type: ${s.type}
    depends_on: [${(s.depends_on || []).join(", ")}]
    config:
      url: "${s.config?.url || ""}"
      method: ${s.config?.method || "GET"}`).join("\n")}`;
      setNewYaml(yaml);
    }
    setYamlError("");
    setDialogOpen(true);
  };

  const handleCreate = async () => {
    if (!newName.trim()) { toast.error("名称不能为空"); return; }
    const err = validateYaml(newYaml);
    if (err) { setYamlError(err); toast.error("YAML 格式错误"); return; }
    setSaving(true);
    try {
      if (editingName) {
        // 编辑模式：先删后建
        await deletePipeline(editingName);
      }
      await createPipeline(newName.trim(), newYaml as unknown as Record<string, unknown>);
      toast.success(editingName ? `Pipeline "${newName}" 已更新` : `Pipeline "${newName}" 已创建`);
      setDialogOpen(false);
      setNewName("");
      setNewYaml(EXAMPLE_YAML);
      setEditingName(null);
      load();
    } catch (e) {
      toast.error(`${editingName ? "更新" : "创建"}失败: ${errMsg(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!deleteName) return;
    try {
      await deletePipeline(deleteName);
      toast.success(`"${deleteName}" 已删除`);
      setDeleteName(null);
      load();
    } catch (e) {
      toast.error(`删除失败: ${errMsg(e)}`);
    }
  };

  const openTest = (name: string) => {
    setTestName(name);
    setTestParams("{}");
    setTestSession("");
    setTestEnv("");
    setTestDryRun(false);
    setTestTimeout("");
    setTestResult(null);
    setTestDialogOpen(true);
  };

  const handleTest = async () => {
    setTestRunning(true);
    setTestResult(null);
    try {
      let params: Record<string, string> = {};
      try { params = JSON.parse(testParams); } catch {
        toast.error("Params JSON 格式错误");
        setTestRunning(false);
        return;
      }
      let session: Record<string, unknown> | undefined;
      if (testSession.trim()) {
        try { session = JSON.parse(testSession); } catch {
          toast.error("Session JSON 格式错误");
          setTestRunning(false);
          return;
        }
      }
      let env: Record<string, unknown> | undefined;
      if (testEnv.trim()) {
        try { env = JSON.parse(testEnv); } catch {
          toast.error("Env JSON 格式错误");
          setTestRunning(false);
          return;
        }
      }
      const result = await testPipeline(testName, {
        params,
        session,
        env,
        dry_run: testDryRun,
        timeout_override: testTimeout.trim() || undefined,
      });
      setTestResult(result as Record<string, unknown>);
    } catch (e) {
      toast.error(`测试失败: ${errMsg(e)}`);
    } finally {
      setTestRunning(false);
    }
  };

  const countSteps = (def: PipelineDef) => def.steps?.length ?? 0;
  const getTimeout = (def: PipelineDef) => def.strategy?.timeout ?? "?";

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Pipelines</h1>
          <p className="text-sm text-muted-foreground mt-1">管理编排流程定义</p>
        </div>
        <Dialog open={dialogOpen} onOpenChange={setDialogOpen}>
          <DialogTrigger asChild>
            <Button onClick={openCreate}>
              <Plus className="mr-2 h-4 w-4" /> 创建 Pipeline
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-2xl">
            <DialogHeader>
              <DialogTitle>{editingName ? `编辑 ${editingName}` : "创建 Pipeline"}</DialogTitle>
            </DialogHeader>
            <div className="grid gap-4 py-4">
              <div className="space-y-2">
                <Label>Pipeline 名称 *</Label>
                <Input
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  placeholder="例如 my_pipeline"
                  disabled={!!editingName}
                />
              </div>
              <div className="space-y-2">
                <Label>YAML 定义</Label>
                <Textarea
                  className={cn("min-h-[300px] font-mono text-sm", yamlError && "border-destructive")}
                  value={newYaml}
                  onChange={(e) => { setNewYaml(e.target.value); setYamlError(""); }}
                />
                {yamlError && (
                  <p className="text-xs text-destructive">{yamlError}</p>
                )}
              </div>
            </div>
            <div className="flex justify-end gap-2">
              <Button variant="outline" onClick={() => setDialogOpen(false)}>取消</Button>
              <Button onClick={handleCreate} disabled={saving}>
                {saving ? "保存中…" : editingName ? "更新" : "创建"}
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
                <TableHead>名称</TableHead>
                <TableHead>步骤数</TableHead>
                <TableHead>超时</TableHead>
                <TableHead>错误处理</TableHead>
                <TableHead>步骤预览</TableHead>
                <TableHead className="w-20">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {loading ? (
                <TableSkeleton rows={5} cols={6} />
              ) : Object.keys(pipelines).length === 0 ? (
                <TableRow>
                  <TableCell colSpan={6} className="text-center text-muted-foreground py-8">
                    暂无 Pipeline，点击「创建 Pipeline」开始定义
                  </TableCell>
                </TableRow>
              ) : (
                Object.entries(pipelines).map(([name, def]) => (
                  <TableRow key={name}>
                    <TableCell className="font-medium">
                      <GitFork className="mr-2 inline h-4 w-4 text-muted-foreground" />
                      {name}
                    </TableCell>
                    <TableCell>
                      <Badge variant="secondary">{countSteps(def)} 步</Badge>
                    </TableCell>
                    <TableCell>
                      <Clock className="mr-1 inline h-3 w-3 text-muted-foreground" />
                      {getTimeout(def)}
                    </TableCell>
                    <TableCell>
                      <Badge variant={def.strategy?.error_handling === "continue" ? "secondary" : "default"}>
                        {def.strategy?.error_handling ?? "fail_fast"}
                      </Badge>
                    </TableCell>
                    <TableCell>
                      <div className="flex items-center gap-1 flex-wrap">
                        {def.steps?.slice(0, 4).map((s: StepDef, i: number) => (
                          <span key={s.id} className="flex items-center gap-1">
                            <Badge variant="outline" className="text-xs">{s.id}</Badge>
                            {i < Math.min(def.steps.length, 4) - 1 && (
                              <ArrowRight className="h-3 w-3 text-muted-foreground" />
                            )}
                          </span>
                        ))}
                        {(def.steps?.length ?? 0) > 4 && (
                          <span className="text-xs text-muted-foreground">
                            +{def.steps.length - 4}
                          </span>
                        )}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button variant="ghost" size="icon" onClick={() => openTest(name)}>
                              <Play className="h-4 w-4" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>测试</TooltipContent>
                        </Tooltip>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button variant="ghost" size="icon" onClick={() => openEdit(name)}>
                              <Pencil className="h-4 w-4" />
                            </Button>
                          </TooltipTrigger>
                          <TooltipContent>编辑</TooltipContent>
                        </Tooltip>
                        <Tooltip>
                          <TooltipTrigger asChild>
                            <Button
                              variant="ghost"
                              size="icon"
                              onClick={() => setDeleteName(name)}
                              className="text-destructive hover:text-destructive"
                            >
                              <Trash2 className="h-4 w-4" />
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

      {/* Test Pipeline Dialog */}
      <Dialog open={testDialogOpen} onOpenChange={setTestDialogOpen}>
        <DialogContent className="max-w-2xl max-h-[85vh] overflow-y-auto">
          <DialogHeader>
            <DialogTitle>测试 Pipeline: {testName}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4 py-4">
            <div className="space-y-2">
              <Label>参数（JSON）</Label>
              <Textarea
                className="min-h-[60px] font-mono text-sm"
                value={testParams}
                onChange={(e) => setTestParams(e.target.value)}
                placeholder='{"userId": "123"}'
              />
            </div>
            <div className="space-y-2">
              <Label>模拟 Session（JSON，可选）</Label>
              <Textarea
                className="min-h-[60px] font-mono text-sm"
                value={testSession}
                onChange={(e) => setTestSession(e.target.value)}
                placeholder='{"sub": "user-123", "access_token": "..."}'
              />
            </div>
            <div className="space-y-2">
              <Label>模拟环境变量（JSON，可选）</Label>
              <Textarea
                className="min-h-[60px] font-mono text-sm"
                value={testEnv}
                onChange={(e) => setTestEnv(e.target.value)}
                placeholder='{"API_KEY": "secret"}'
              />
            </div>
            <div className="flex items-center gap-6">
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={testDryRun}
                  onChange={(e) => setTestDryRun(e.target.checked)}
                  className="rounded"
                />
                Dry Run（仅验证不执行）
              </label>
              <div className="flex items-center gap-2">
                <Label className="text-sm whitespace-nowrap">超时覆盖</Label>
                <Input
                  className="w-32"
                  value={testTimeout}
                  onChange={(e) => setTestTimeout(e.target.value)}
                  placeholder="如 30s"
                />
              </div>
            </div>

            <Button onClick={handleTest} disabled={testRunning} className="w-full">
              {testRunning ? (
                <><Loader2 className="mr-2 h-4 w-4 animate-spin" /> 执行中…</>
              ) : (
                <><Play className="mr-2 h-4 w-4" /> 执行测试</>
              )}
            </Button>

            {testResult && (
              <div className="space-y-3 border-t pt-4">
                <div className="flex items-center gap-4 flex-wrap">
                  <Badge variant={testResult.status === 200 ? "default" : "destructive"}>
                    状态: {String(testResult.status)}
                  </Badge>
                  <Badge variant="secondary">
                    总耗时: {String(testResult.total_duration_ms ?? "?")} ms
                  </Badge>
                  {testResult.session_injected ? (
                    <Badge variant="default">Session 已注入</Badge>
                  ) : null}
                  {testResult.env_injected ? (
                    <Badge variant="default">Env 已注入</Badge>
                  ) : null}
                </div>

                {Array.isArray(testResult.steps) && (testResult.steps as Array<Record<string, unknown>>).length > 0 && (
                  <div className="space-y-2">
                    <Label>Step 级详情</Label>
                    <div className="rounded-lg border">
                      <Table>
                        <TableHeader>
                          <TableRow>
                            <TableHead>Step ID</TableHead>
                            <TableHead>类型</TableHead>
                            <TableHead>状态</TableHead>
                            <TableHead>耗时</TableHead>
                            <TableHead>缓存</TableHead>
                          </TableRow>
                        </TableHeader>
                        <TableBody>
                          {(testResult.steps as Array<Record<string, unknown>>).map((step: Record<string, unknown>) => (
                            <TableRow key={String(step.id)}>
                              <TableCell className="font-medium font-mono text-xs">{String(step.id)}</TableCell>
                              <TableCell>
                                <Badge variant="outline" className="text-xs">{String(step.type)}</Badge>
                              </TableCell>
                              <TableCell>
                                {step.status === 200 ? (
                                  <CheckCircle className="h-4 w-4 text-green-500" />
                                ) : step.error ? (
                                  <XCircle className="h-4 w-4 text-red-500" />
                                ) : (
                                  <AlertTriangle className="h-4 w-4 text-yellow-500" />
                                )}
                              </TableCell>
                              <TableCell className="text-sm">
                                {String(step.duration_ms ?? "?")} ms
                              </TableCell>
                              <TableCell>
                                {step.cache_hit ? (
                                  <Badge variant="secondary" className="text-xs">HIT</Badge>
                                ) : (
                                  <span className="text-xs text-muted-foreground">MISS</span>
                                )}
                              </TableCell>
                            </TableRow>
                          ))}
                        </TableBody>
                      </Table>
                    </div>
                  </div>
                )}

                <details>
                  <summary className="cursor-pointer text-sm font-medium text-muted-foreground">
                    查看完整响应
                  </summary>
                  <pre className="mt-2 rounded-lg bg-muted p-4 text-xs overflow-auto max-h-[200px]">
                    {JSON.stringify(testResult, null, 2)}
                  </pre>
                </details>
              </div>
            )}
          </div>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={deleteName !== null}
        onOpenChange={(open) => { if (!open) setDeleteName(null); }}
        title="删除 Pipeline"
        description={`确定要删除 Pipeline "${deleteName}" 吗？此操作不可撤销。`}
        onConfirm={handleDelete}
        variant="destructive"
      />
    </div>
  );
}
