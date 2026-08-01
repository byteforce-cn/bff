// ============================================================
// 配置管理：YAML 导入 / 导出，可视化分类编辑
// ============================================================

import { useEffect, useState } from "react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { exportConfig, importConfig } from "@/lib/api";
import { Download, Upload, RefreshCw, AlertCircle, CheckCircle2 } from "lucide-react";
import { toast } from "sonner";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function validateYamlBasic(yaml: string): string {
  if (!yaml.trim()) return "";
  const lines = yaml.split("\n");
  for (let i = 0; i < lines.length; i++) {
    if (lines[i].match(/^\t/)) return `第 ${i + 1} 行：YAML 不允许使用 Tab 缩进`;
  }
  return "";
}

export default function ConfigPage() {
  const [yaml, setYaml] = useState("");
  const [loading, setLoading] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [yamlError, setYamlError] = useState("");

  const handleExport = async () => {
    setExporting(true);
    try {
      const text = await exportConfig();
      setYaml(text);
      setYamlError("");
      toast.success("配置已导出");
    } catch (e) {
      toast.error(`导出失败: ${errMsg(e)}`);
    } finally {
      setExporting(false);
    }
  };

  const handleImport = async () => {
    if (!yaml.trim()) {
      toast.error("请先导出配置或粘贴 YAML 内容");
      return;
    }
    const err = validateYamlBasic(yaml);
    if (err) { setYamlError(err); toast.error("YAML 格式错误"); return; }
    setLoading(true);
    try {
      await importConfig(yaml);
      toast.success("配置已导入并热重载");
      setYamlError("");
    } catch (e) {
      toast.error(`导入失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">配置管理</h1>
          <p className="text-sm text-muted-foreground mt-1">
            导入 / 导出 BFF 完整配置，修改后自动热重载
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={handleExport} disabled={exporting}>
            <Download className="mr-2 h-4 w-4" />
            {exporting ? "导出中…" : "导出配置"}
          </Button>
          <Button onClick={handleImport} disabled={loading}>
            <Upload className="mr-2 h-4 w-4" />
            {loading ? "导入中…" : "导入并应用"}
          </Button>
        </div>
      </div>

      <Tabs defaultValue="yaml" className="w-full">
        <TabsList>
          <TabsTrigger value="yaml">YAML 编辑器</TabsTrigger>
          <TabsTrigger value="help">配置说明</TabsTrigger>
        </TabsList>

        <TabsContent value="yaml" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle className="text-base">YAML 配置</CardTitle>
              <CardDescription>
                点击「导出配置」加载当前配置，编辑后点击「导入并应用」即可热重载。
                敏感字段（如 client_secret）会自动脱敏，导入时需填回真实值。
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Textarea
                className={`min-h-[500px] font-mono text-sm ${yamlError ? "border-destructive" : ""}`}
                placeholder="点击「导出配置」加载…"
                value={yaml}
                onChange={(e) => { setYaml(e.target.value); setYamlError(""); }}
              />
              {yamlError && <p className="text-xs text-destructive mt-1">{yamlError}</p>}
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="help" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle className="text-base">配置结构说明</CardTitle>
            </CardHeader>
            <CardContent className="prose prose-sm max-w-none dark:prose-invert">
              <pre className="rounded-lg bg-muted p-4 text-xs overflow-auto">{`# server — 服务端口
server:
  business_port: 8080   # 业务端口（SPA、OIDC、API 代理）
  admin_port: 8443      # 管理端口（管理 UI + API）

# provider — 中间件存储后端
provider:
  session_store: memory  # memory | redis
  cache: memory          # memory | redis
  lock: memory           # memory | redis
  redis_url: ""          # Redis 连接地址

# session — Cookie 与会话配置
session:
  cookie_name: "BFF_SESSION"
  secure: false          # 生产环境需设为 true
  http_only: true
  same_site: "Lax"

# admin — 管理端口安全
admin:
  ip_whitelist:          # IP 白名单（CIDR）
    - "127.0.0.1"
    - "::1"
  auth_mode: token       # token | none
  auth_token: "changeme" # ⚠️ 生产必须修改

# oidc — OIDC 身份提供者
oidc:
  providers:
    - id: keycloak
      display_name: "企业 SSO"
      issuer_url: "https://..."
      client_id: "bff"
      client_secret: "***"
      scopes: ["openid", "profile"]

# pipelines — 编排定义
pipelines:
  my_pipeline:
    strategy:
      timeout: 10s
      error_handling: fail_fast
    steps:
      - id: step1
        type: http_request
        config:
          url: "http://..."
          method: GET

# routes — 反向代理路由
routes:
  - path_prefix: "/api/users"
    upstream: "http://user-service"
    strip_prefix: false
    auth_required: true`}</pre>
            </CardContent>
          </Card>

          <Alert>
            <AlertCircle className="h-4 w-4" />
            <AlertDescription>
              <strong>提示：</strong>导入配置会立即生效（热重载），无需重启 BFF。
              导入前请确认 YAML 格式正确，否则会返回 422 错误且不会应用。
            </AlertDescription>
          </Alert>
        </TabsContent>
      </Tabs>
    </div>
  );
}
