// ============================================================
// KeyValueEditor：动态键值对表单编辑器
// ============================================================

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Plus, Trash2 } from "lucide-react";

interface KeyValueRow {
  key: string;
  value: string;
}

interface KeyValueEditorProps {
  title: string;
  entries: Record<string, string>;
  onChange: (entries: Record<string, string>) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}

export function KeyValueEditor({
  title,
  entries,
  onChange,
  keyPlaceholder = "目标变量",
  valuePlaceholder = "来源路径",
}: KeyValueEditorProps) {
  const rows: KeyValueRow[] = Object.entries(entries).map(([key, value]) => ({
    key,
    value,
  }));

  const updateRow = (index: number, field: "key" | "value", newVal: string) => {
    const newRows = [...rows];
    newRows[index] = { ...newRows[index], [field]: newVal };
    const result: Record<string, string> = {};
    for (const r of newRows) {
      if (r.key.trim()) result[r.key.trim()] = r.value;
    }
    onChange(result);
  };

  const addRow = () => {
    onChange({ ...entries, "": "" });
  };

  const deleteRow = (index: number) => {
    const result: Record<string, string> = {};
    rows.forEach((r, i) => {
      if (i !== index && r.key.trim()) result[r.key.trim()] = r.value;
    });
    onChange(result);
  };

  // Filter out empty-key rows for display but keep them editable
  const displayRows: KeyValueRow[] = rows.length > 0 ? rows : [{ key: "", value: "" }];

  return (
    <div className="space-y-2 rounded-lg border p-3">
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-muted-foreground">{title}</span>
        <Button variant="ghost" size="icon" className="h-6 w-6" onClick={addRow}>
          <Plus className="h-3 w-3" />
        </Button>
      </div>
      {displayRows.length === 0 || (displayRows.length === 1 && displayRows[0].key === "") ? (
        <p className="text-xs text-muted-foreground py-1">暂无映射，点击 + 添加</p>
      ) : (
        <div className="space-y-1">
          <div className="flex gap-2 text-xs text-muted-foreground px-1">
            <span className="flex-1">目标变量</span>
            <span className="flex-1">来源路径</span>
            <span className="w-8" />
          </div>
          {rows.map((row, i) => (
            <div key={i} className="flex gap-1 items-center">
              <Input
                className="h-8 text-xs flex-1"
                value={row.key}
                onChange={(e) => updateRow(i, "key", e.target.value)}
                placeholder={keyPlaceholder}
              />
              <Input
                className="h-8 text-xs flex-1"
                value={row.value}
                onChange={(e) => updateRow(i, "value", e.target.value)}
                placeholder={valuePlaceholder}
              />
              <Button
                variant="ghost"
                size="icon"
                className="h-8 w-8 shrink-0"
                onClick={() => deleteRow(i)}
              >
                <Trash2 className="h-3 w-3 text-muted-foreground" />
              </Button>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
