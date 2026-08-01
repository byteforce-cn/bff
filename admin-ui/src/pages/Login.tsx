// ============================================================
// 登录页：通过 Admin Token 登录
// ============================================================

import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { useAuth } from "@/hooks/useAuth";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Shield, Key, Eye, EyeOff } from "lucide-react";

export default function LoginPage() {
  const { isAuthenticated, login } = useAuth();
  const navigate = useNavigate();
  const [tokenInput, setTokenInput] = useState("");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [showToken, setShowToken] = useState(false);

  // 已登录则跳转
  if (isAuthenticated) {
    navigate("/dashboard", { replace: true });
    return null;
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");

    if (!tokenInput.trim()) {
      setError("请输入管理 Token");
      return;
    }

    setLoading(true);
    try {
      const ok = await login(tokenInput.trim());
      if (ok) {
        navigate("/dashboard", { replace: true });
      } else {
        setError("Token 验证失败，请检查后重试");
      }
    } catch {
      setError("网络错误，请确认 BFF 管理端口可访问");
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex min-h-screen items-center justify-center bg-muted/40 p-4">
      <Card className="w-full max-w-sm">
        <CardHeader className="space-y-1 text-center">
          <div className="mx-auto flex h-12 w-12 items-center justify-center rounded-xl bg-primary">
            <Shield className="h-6 w-6 text-primary-foreground" />
          </div>
          <CardTitle className="text-xl">BFF 管理控制台</CardTitle>
          <CardDescription>请输入管理 Token 以登录</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <div className="space-y-2">
              <div className="relative">
                <Key className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  type={showToken ? "text" : "password"}
                  placeholder="Admin Token"
                  className="pl-9 pr-9"
                  value={tokenInput}
                  onChange={(e) => setTokenInput(e.target.value)}
                  autoFocus
                />
                <button
                  type="button"
                  className="absolute right-3 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
                  onClick={() => setShowToken(!showToken)}
                >
                  {showToken ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
                </button>
              </div>
            </div>
            <Button type="submit" className="w-full" disabled={loading}>
              {loading ? "验证中…" : "登录"}
            </Button>
          </form>
          <p className="mt-4 text-center text-xs text-muted-foreground">
            默认 Token 为 <code className="rounded bg-muted px-1">changeme</code>
            ，生产环境请务必修改
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
