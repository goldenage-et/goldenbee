import type { TaskUsage } from "@goldenbee/sdk"

function usageNumber(
  usage: TaskUsage | undefined,
  key: string,
): number | undefined {
  const value = usage?.[key]
  return typeof value === "number" && Number.isFinite(value) ? value : undefined
}

export function UsageFacts({
  usage,
  emptyMessage = "No usage counters were reported. Missing values are not treated as zero.",
}: {
  usage: TaskUsage | null | undefined
  emptyMessage?: string
}) {
  const rows: Array<[string, number | undefined]> = [
    ["Prompt tokens", usageNumber(usage ?? undefined, "prompt_tokens")],
    ["Candidate tokens", usageNumber(usage ?? undefined, "candidate_tokens")],
    ["Estimated tokens", usageNumber(usage ?? undefined, "estimated_tokens")],
    [
      "Estimated cost (micro-units)",
      usageNumber(usage ?? undefined, "estimated_cost_micros"),
    ],
    ["Model calls", usageNumber(usage ?? undefined, "model_calls")],
    ["Tool calls", usageNumber(usage ?? undefined, "tool_calls")],
  ]
  if (rows.every(([, value]) => value === undefined)) {
    return <p className="text-meta text-muted-foreground">{emptyMessage}</p>
  }
  return (
    <dl className="grid gap-1">
      {rows
        .filter(([, value]) => value !== undefined)
        .map(([label, value]) => (
          <div
            key={label}
            className="grid grid-cols-[auto_1fr] gap-3 text-micro"
          >
            <dt className="text-dim">{label}</dt>
            <dd className="text-right text-foreground tabular-nums">{value}</dd>
          </div>
        ))}
    </dl>
  )
}
