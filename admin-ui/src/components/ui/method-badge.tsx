// ============================================================
// MethodBadge：HTTP Method 彩色标签
// ============================================================

import { Badge } from "@/components/ui/badge";
import { cn } from "@/lib/utils";

const METHOD_COLORS: Record<string, string> = {
  GET: "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-400",
  POST: "bg-blue-100 text-blue-800 dark:bg-blue-900/30 dark:text-blue-400",
  PUT: "bg-amber-100 text-amber-800 dark:bg-amber-900/30 dark:text-amber-400",
  PATCH: "bg-orange-100 text-orange-800 dark:bg-orange-900/30 dark:text-orange-400",
  DELETE: "bg-red-100 text-red-800 dark:bg-red-900/30 dark:text-red-400",
  HEAD: "bg-purple-100 text-purple-800 dark:bg-purple-900/30 dark:text-purple-400",
  OPTIONS: "bg-gray-100 text-gray-800 dark:bg-gray-900/30 dark:text-gray-400",
};

interface MethodBadgeProps {
  method: string;
}

export function MethodBadge({ method }: MethodBadgeProps) {
  const colorClass = METHOD_COLORS[method.toUpperCase()] || "bg-muted text-muted-foreground";

  return (
    <Badge
      variant="outline"
      className={cn("font-mono text-[10px] px-1.5 py-0 leading-normal", colorClass)}
    >
      {method.toUpperCase()}
    </Badge>
  );
}

/** 渲染一组 MethodBadge，空数组显示 ANY */
export function MethodBadgeList({ methods }: { methods: string[] }) {
  if (!methods || methods.length === 0) {
    return (
      <Badge variant="secondary" className="text-[10px] px-1.5 py-0">
        ANY
      </Badge>
    );
  }
  return (
    <div className="flex gap-0.5 flex-wrap">
      {methods.map((m) => (
        <MethodBadge key={m} method={m} />
      ))}
    </div>
  );
}
