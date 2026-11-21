import { Streamdown } from "streamdown"
import { cn } from "cn"

/**
 * Markdown renderer for assistant replies.
 *
 * Completed replies use static mode; streaming replies use Streamdown's
 * incomplete-markdown handling. Raw HTML is not enabled, so model output stays
 * escaped text unless it is valid Markdown.
 */
export function Markdown({
  children,
  streaming = false,
  className,
}: {
  children: string
  streaming?: boolean
  className?: string
}) {
  return (
    <Streamdown
      mode={streaming ? "streaming" : "static"}
      isAnimating={streaming}
      className={cn("text-sm leading-relaxed", className)}
    >
      {children}
    </Streamdown>
  )
}
