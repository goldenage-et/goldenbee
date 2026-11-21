import { SearchIcon } from "lucide-react"
import {
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react"
import { createPortal } from "react-dom"
import { cn } from "cn"

export interface PaletteAction {
  id: string
  label: string
  group: "Actions" | "Groups" | "Sessions" | "Navigation" | "Settings"
  hint?: string
  run(): void
}

export interface CommandPaletteProps {
  open: boolean
  onClose(): void
  actions: PaletteAction[]
}

/**
 * Local command palette. It searches Client actions, session groups, and known
 * local chats only; there is no Gateway search route in this release.
 */
export function CommandPalette({ open, onClose, actions }: CommandPaletteProps) {
  const [query, setQuery] = useState("")
  const [index, setIndex] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const restoreFocus = useRef<HTMLElement | null>(null)
  const listId = useId()

  useEffect(() => {
    if (!open) {
      return
    }
    restoreFocus.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null
    // Opening the palette is an external event; a fresh query and selection are
    // part of entering that mode.
    // oxlint-disable-next-line react/set-state-in-effect
    setQuery("")
    // oxlint-disable-next-line react/set-state-in-effect
    setIndex(0)
    const timer = window.setTimeout(() => inputRef.current?.focus(), 0)
    return () => {
      window.clearTimeout(timer)
      const target = restoreFocus.current
      window.setTimeout(() => target?.focus(), 0)
    }
  }, [open])

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase()
    if (!needle) {
      return actions
    }
    return actions.filter((action) =>
      `${action.label} ${action.hint ?? ""}`.toLowerCase().includes(needle),
    )
  }, [actions, query])

  const groups = useMemo(() => {
    const order: PaletteAction["group"][] = [
      "Actions",
      "Groups",
      "Sessions",
      "Navigation",
      "Settings",
    ]
    return order
      .map((group) => ({
        group,
        items: filtered.filter((action) => action.group === group),
      }))
      .filter((entry) => entry.items.length > 0)
  }, [filtered])

  const activeIndex =
    filtered.length === 0 ? -1 : Math.min(Math.max(index, 0), filtered.length - 1)
  const activeAction = activeIndex >= 0 ? filtered[activeIndex] : undefined

  function run(action: PaletteAction | undefined): void {
    if (!action) {
      return
    }
    onClose()
    action.run()
  }

  if (!open) {
    return null
  }

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 p-4 pt-[12vh]"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) {
          onClose()
        }
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        className="w-full max-w-xl overflow-hidden rounded-[11px] border border-line bg-surface-2 shadow-2xl"
      >
        <div className="flex items-center gap-2 border-b border-line px-3">
          <SearchIcon className="size-4 shrink-0 text-dim" aria-hidden="true" />
          <input
            ref={inputRef}
            role="combobox"
            aria-label="Command"
            aria-autocomplete="list"
            aria-expanded="true"
            aria-controls={listId}
            aria-activedescendant={
              activeAction ? `${listId}-${activeAction.id}` : undefined
            }
            autoComplete="off"
            spellCheck={false}
            placeholder="Type a command, group, or local session…"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value)
              setIndex(0)
            }}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                event.preventDefault()
                onClose()
                return
              }
              if (event.key === "Tab") {
                // The palette is a single-focus modal; keep focus on the input.
                event.preventDefault()
                return
              }
              if (event.key === "ArrowDown") {
                event.preventDefault()
                if (filtered.length > 0) {
                  setIndex((activeIndex + 1) % filtered.length)
                }
                return
              }
              if (event.key === "ArrowUp") {
                event.preventDefault()
                if (filtered.length > 0) {
                  setIndex(
                    (activeIndex - 1 + filtered.length) % filtered.length,
                  )
                }
                return
              }
              if (event.key === "Home") {
                event.preventDefault()
                setIndex(0)
                return
              }
              if (event.key === "End") {
                event.preventDefault()
                setIndex(Math.max(filtered.length - 1, 0))
                return
              }
              if (event.key === "Enter") {
                event.preventDefault()
                run(activeAction)
              }
            }}
            className="h-11 min-w-0 flex-1 bg-transparent text-body outline-none placeholder:text-dim"
          />
          <kbd className="rounded border border-line bg-surface px-1.5 py-0.5 text-micro text-dim">
            Esc
          </kbd>
        </div>

        <div
          id={listId}
          role="listbox"
          aria-label="Commands"
          className="max-h-[min(24rem,55vh)] overflow-y-auto p-1.5"
        >
          {filtered.length === 0 ? (
            <p role="status" className="px-2 py-3 text-meta text-muted-foreground">
              No matching commands. This palette searches local Client state only.
            </p>
          ) : (
            groups.map((entry) => (
              <div key={entry.group} className="mb-1">
                <p
                  role="presentation"
                  className="px-2 py-1 text-micro font-bold tracking-[0.14em] text-dim uppercase"
                >
                  {entry.group}
                </p>
                {entry.items.map((action) => {
                  const position = filtered.indexOf(action)
                  const selected = position === activeIndex
                  return (
                    <button
                      key={action.id}
                      type="button"
                      role="option"
                      id={`${listId}-${action.id}`}
                      aria-selected={selected}
                      onMouseEnter={() => setIndex(position)}
                      onClick={() => run(action)}
                      className={cn(
                        "flex w-full items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-body",
                        selected
                          ? "bg-gold/10 text-foreground"
                          : "text-muted-foreground hover:bg-surface-3",
                      )}
                    >
                      <span
                        aria-hidden="true"
                        className={cn(
                          "shrink-0 text-meta",
                          selected ? "text-gold" : "text-dim",
                        )}
                      >
                        ⌁
                      </span>
                      <span className="min-w-0 flex-1 truncate">
                        {action.label}
                      </span>
                      {action.hint ? (
                        <span className="shrink-0 text-micro text-dim">
                          {action.hint}
                        </span>
                      ) : null}
                    </button>
                  )
                })}
              </div>
            ))
          )}
        </div>
      </div>
    </div>,
    document.body,
  )
}
