// ============================================================
// TableSkeleton：表格加载骨架屏
// ============================================================

import { cn } from "@/lib/utils";

interface TableSkeletonProps {
  rows?: number;
  cols?: number;
  className?: string;
}

export function TableSkeleton({ rows = 5, cols = 4, className }: TableSkeletonProps) {
  return (
    <>
      {Array.from({ length: rows }).map((_, row) => (
        <tr key={row} className={cn("animate-pulse", className)}>
          {Array.from({ length: cols }).map((_, col) => (
            <td key={col} className="px-4 py-3">
              <div
                className="h-4 rounded bg-muted"
                style={{ width: `${60 + Math.random() * 30}%` }}
              />
            </td>
          ))}
        </tr>
      ))}
    </>
  );
}
