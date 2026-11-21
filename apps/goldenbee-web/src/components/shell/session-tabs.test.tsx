import { render, screen } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it, vi } from "vitest"

import { SessionTabs } from "@/components/shell/session-tabs"

const group = { id: "group-a", name: "Sessions", chatIds: ["chat-a"] }

describe("session tabs", () => {
  it("renders a transient file preview tab and closes it", async () => {
    const user = userEvent.setup()
    const onSelectPreview = vi.fn()
    const onClosePreview = vi.fn()
    render(
      <SessionTabs
        groups={[group]}
        activeGroupId="group-a"
        backgroundChatIds={[]}
        sidebarOpen
        onToggleSidebar={() => undefined}
        onSelectGroup={() => undefined}
        onNewSession={() => undefined}
        onNewGroup={() => undefined}
        onRequestRename={() => undefined}
        onRequestClose={() => undefined}
        preview={{ path: "notes/index.html", name: "index.html" }}
        previewActive
        onSelectPreview={onSelectPreview}
        onClosePreview={onClosePreview}
      />,
    )

    expect(screen.getByRole("tab", { name: /index\.html/ })).toHaveAttribute(
      "aria-selected",
      "true",
    )
    await user.click(screen.getByRole("tab", { name: /index\.html/ }))
    expect(onSelectPreview).toHaveBeenCalled()
    await user.click(screen.getByRole("button", { name: /Close file preview/ }))
    expect(onClosePreview).toHaveBeenCalled()
  })
})
