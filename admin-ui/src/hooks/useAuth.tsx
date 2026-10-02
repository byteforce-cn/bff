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
import { clearToken, getToken, setToken, verifyToken } from "@/lib/api";

interface AuthContextType {
  token: string | null;
  isAuthenticated: boolean;
  isLoading: boolean;
  login: (token: string) => Promise<boolean>;
  logout: () => void;
}

const AuthContext = createContext<AuthContextType | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  // token 存取走 api.ts 的 sessionStorage 实现（关标签页失效）
  const [token, setTokenState] = useState<string | null>(() => getToken() || null);
  const [isLoading, setIsLoading] = useState(true);

  // 启动时校验已有 token
  useEffect(() => {
    const stored = getToken();
    if (stored) {
      verifyToken()
        .then((ok) => {
          if (!ok) {
            clearToken();
            setTokenState(null);
          }
        })
        .catch(() => {
          clearToken();
          setTokenState(null);
        })
        .finally(() => setIsLoading(false));
    } else {
      setIsLoading(false);
    }
  }, []);

  const login = useCallback(async (newToken: string): Promise<boolean> => {
    // 临时保存以验证
    const prev = getToken();
    setToken(newToken);
    try {
      const ok = await verifyToken();
      if (ok) {
        setTokenState(newToken);
        return true;
      }
      if (prev) {
        setToken(prev);
      } else {
        clearToken();
      }
      return false;
    } catch {
      if (prev) {
        setToken(prev);
      } else {
        clearToken();
      }
      return false;
    }
  }, []);

  const logout = useCallback(() => {
    clearToken();
    setTokenState(null);
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
