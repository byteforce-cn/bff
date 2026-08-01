// ============================================================
// 指标页面：Prometheus 指标展示
// ============================================================

import { useCallback, useEffect, useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { metrics } from "@/lib/api";
import { BarChart3, RefreshCw, Download } from "lucide-react";
import { toast } from "sonner";

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export default function MetricsPage() {
  const [text, setText] = useState("");
  const [loading, setLoading] = useState(true);

  const load = useCallback(async () => {
    try {
      const data = await metrics();
      setText(data as string);
    } catch (e) {
      toast.error(`加载失败: ${errMsg(e)}`);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const handleDownload = () => {
    const blob = new Blob([text], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "bff-metrics.txt";
    a.click();
    URL.revokeObjectURL(url);
  };

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">指标</h1>
          <p className="text-sm text-muted-foreground mt-1">
            Prometheus 文本格式指标（可接入 Grafana）
          </p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={load} disabled={loading}>
            <RefreshCw className={`mr-2 h-4 w-4 ${loading ? "animate-spin" : ""}`} />
            刷新
          </Button>
          <Button variant="outline" onClick={handleDownload} disabled={!text}>
            <Download className="mr-2 h-4 w-4" />
            下载
          </Button>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">
            <BarChart3 className="mr-2 inline h-4 w-4" />
            Prometheus Metrics
          </CardTitle>
        </CardHeader>
        <CardContent>
          {loading ? (
            <div className="flex items-center justify-center py-16">
              <RefreshCw className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : (
            <pre className="max-h-[600px] overflow-auto rounded-lg bg-muted p-4 text-xs font-mono">
              {text || "暂无指标数据"}
            </pre>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
