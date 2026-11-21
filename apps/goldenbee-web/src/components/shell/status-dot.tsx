import { cn } from "@/lib/utils"

export type StatusTone = "positive" | "danger" | "info" | "profile" | "gold" | "dim"

const toneClasses: Record<StatusTone, string> = {
  positive: "bg-positive",
  danger: "bg-danger",
  info: "bg-info",
  profile: "bg-profile",
  gold: "bg-gold",
  dim: "bg-dim",
}

/**
 * Status is always a dot plus text, never colour alone.
 */
export function StatusDot({
  tone,
  children,
  className,
}: {
  tone: StatusTone
  children: React.ReactNode
  className?: string
}) {
  return (
    <span className={cn("inline-flex min-w-0 items-center gap-1.5", className)}>
      <span
        aria-hidden="true"
        className={cn("size-1.5 shrink-0 rounded-full", toneClasses[tone])}
      />
      <span className="min-w-0 truncate">{children}</span>
    </span>
  )
}
