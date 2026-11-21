import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen } from "@testing-library/react"
import { describe, expect, it } from "vitest"
import type { ReactNode } from "react"

import { WorkspaceFilePreview } from "@/features/inspector/workspace-file-preview"

function renderPreview(
  file: { path: string; text: string; binary?: boolean } = {
    path: "index.html",
    text: "<h1>hi</h1>",
  },
) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  })
  function wrapper({ children }: { children: ReactNode }) {
    return (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )
  }
  return render(
    <WorkspaceFilePreview
      client={{
        async workspaceFile(_worker, path) {
          return {
            path,
            kind: "file",
            size: file.text.length,
            modified_at: 1,
            binary: file.binary ?? false,
            truncated: false,
            text: file.binary ? undefined : file.text,
          }
        },
      }}
      worker="general-a"
      path={file.path}
      liveVersion={0}
      liveStatus="live"
      onClose={() => undefined}
    />,
    { wrapper },
  )
}

describe("workspace file preview", () => {
  it("renders the bounded text preview and live status", async () => {
    renderPreview()
    expect(await screen.findByText("<h1>hi</h1>")).toBeInTheDocument()
    expect(screen.getByText("Live")).toBeInTheDocument()
    expect(screen.getAllByText("index.html").length).toBeGreaterThan(0)
  })

  it("does not render binary bytes", async () => {
    renderPreview({ path: "image.png", text: "not-bytes", binary: true })
    expect(await screen.findByText("Binary content is not previewed.")).toBeInTheDocument()
    expect(screen.queryByText("not-bytes")).not.toBeInTheDocument()
  })
})
