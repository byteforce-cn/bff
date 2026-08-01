// ============================================================
// 认证上下文：管理 Token 登录状态
// ============================================================

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import { verifyToken } from "@/lib/api";

interface AuthContextType {
  token: string | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  login: (token: string) => Promise<boolean>;
  logout: () => void;
}

const AuthContext = createContext<AuthContextType | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState<string | null>(() =>
    localStorage.getItem("bff_admin_token")
  );
  const [isLoading, setIsLoading] = useState(true);

  // 启动时校验已有 token
  useEffect(() => {
    const stored = localStorage.getItem("bff_admin_token");
    if (stored) {
      verifyToken()
        .then((ok) => {
          if (!ok) {
            localStorage.removeItem("bff_admin_token");
            setToken(null);
          }
        })
        .catch(() => {
          localStorage.removeItem("bff_admin_token");
          setToken(null);
        })
        .finally(() => setIsLoading(false));
    } else {
      setIsLoading(false);
    }
  }, []);

  const login = useCallback(async (newToken: string): Promise<boolean> => {
    // 临时保存以验证
    const prev = localStorage.getItem("bff_admin_token");
    localStorage.setItem("bff_admin_token", newToken);
    try {
      const ok = await verifyToken();
      if (ok) {
        setToken(newToken);
        return true;
      }
      localStorage.setItem("bff_admin_token", prev || "");
      return false;
    } catch {
      localStorage.setItem("bff_admin_token", prev || "");
      return false;
    }
  }, []);

  const logout = useCallback(() => {
    localStorage.removeItem("bff_admin_token");
    setToken(null);
  }, []);

  return (
    <AuthContext.Provider
      value={{
        token,
        isAuthenticated: !!token,
        isLoading,
        login,
        logout,
      }}
    >
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth(): AuthContextType {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used within AuthProvider");
  return ctx;
}
