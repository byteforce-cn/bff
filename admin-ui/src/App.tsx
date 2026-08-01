// ============================================================
// 应用根组件：路由定义
// ============================================================

import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import { AuthProvider } from "@/hooks/useAuth";
import { AuthGuard } from "@/components/AuthGuard";
import { Layout } from "@/components/Layout";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";

import LoginPage from "@/pages/Login";
import DashboardPage from "@/pages/Dashboard";
import ConfigPage from "@/pages/Config";
import ProvidersPage from "@/pages/Providers";
import PipelinesPage from "@/pages/Pipelines";
import ScriptsPage from "@/pages/Scripts";
import SessionsPage from "@/pages/Sessions";
import RoutesPage from "@/pages/Routes";
import MetricsPage from "@/pages/Metrics";

export default function App() {
  return (
    <BrowserRouter>
      <AuthProvider>
        <TooltipProvider delayDuration={300}>
          <ErrorBoundary>
            <Routes>
              {/* 公开路由 */}
              <Route path="/login" element={<LoginPage />} />

              {/* 受保护路由 */}
              <Route element={<AuthGuard />}>
                <Route element={<Layout />}>
                  <Route path="/dashboard" element={<DashboardPage />} />
                  <Route path="/config" element={<ConfigPage />} />
                  <Route path="/providers" element={<ProvidersPage />} />
                  <Route path="/pipelines" element={<PipelinesPage />} />
                  <Route path="/scripts" element={<ScriptsPage />} />
                  <Route path="/sessions" element={<SessionsPage />} />
                  <Route path="/routes" element={<RoutesPage />} />
                  <Route path="/metrics" element={<MetricsPage />} />
                </Route>
              </Route>

              {/* 默认重定向 */}
              <Route path="/" element={<Navigate to="/dashboard" replace />} />
              <Route path="*" element={<Navigate to="/dashboard" replace />} />
            </Routes>
          </ErrorBoundary>
          <Toaster position="top-right" richColors />
        </TooltipProvider>
      </AuthProvider>
    </BrowserRouter>
  );
}
