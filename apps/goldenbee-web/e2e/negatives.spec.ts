import { expect, test } from "@playwright/test"

// A malformed SSE frame and a cursor-expired replay cannot be induced through
// the real Gateway without a fault-injection route. Those negatives live at the
// parser/reducer seam instead:
//   packages/goldenbee-sdk/src/sse.test.ts
//   apps/goldenbee-web/src/features/chat/chat-reducer.test.ts
//   apps/goldenbee-web/src/features/chat/chat-recovery.test.ts

import {
  AGENT,
  connect,
  expectNoTokenInUrlLocalStorageOrConsole,
  openChat,
  selectTarget,
  sendPrompt,
  sessionStorageToken,
  trackConsole,
  waitForIdle,
  WORKER,
} from "./support"
import {
  E2E_CALLER_TOKEN,
  E2E_DOWN_TOKEN,
  E2E_INVALID_TOKEN,
  E2E_READONLY_TOKEN,
} from "./tokens"

test("an invalid caller token is rejected without revealing it", async ({ page }) => {
  const consoleLines = trackConsole(page)
  await page.goto("/")
  await page.getByLabel("Caller token").fill(E2E_INVALID_TOKEN)
  await page.getByRole("button", { name: "Connect" }).click()

  await expect(page.getByText("Caller token rejected")).toBeVisible()
  await expect(
    page.getByText(
      "Gateway recognized the request but did not accept this caller token.",
    ),
  ).toBeVisible()
  await expectNoTokenInUrlLocalStorageOrConsole(
    page,
    E2E_INVALID_TOKEN,
    consoleLines,
  )
})

test("a read-only token cannot read the provider administration surface", async ({
  page,
}) => {
  const consoleLines = trackConsole(page)

  await connect(page, E2E_READONLY_TOKEN)
  await page.goto("/#/providers")
  await expect(
    page.getByRole("heading", { name: "Model providers" }),
  ).toBeVisible()
  await expect(page.getByText("Not permitted")).toBeVisible()

  await expectNoTokenInUrlLocalStorageOrConsole(
    page,
    E2E_READONLY_TOKEN,
    consoleLines,
  )
  // The default is memory-only; no reload-time token was written.
  expect(await sessionStorageToken(page)).toBeNull()
})

test("an unavailable Worker is shown honestly and Send stays disabled", async ({
  page,
}) => {
  await connect(page, E2E_DOWN_TOKEN)
  await openChat(page)

  await expect(
    page.getByText("No reachable Worker is visible to this caller."),
  ).toBeVisible()
  await expect(page.getByLabel("Worker")).toContainText("No reachable Worker")
  await expect(page.getByRole("button", { name: "Send message" })).toBeDisabled()
})

test("task and model output is rendered as text, never as HTML", async ({ page }) => {
  const consoleLines = trackConsole(page)

  await connect(page, E2E_CALLER_TOKEN)
  await openChat(page)
  await selectTarget(page, WORKER, AGENT)

  const payload = '<img src=x onerror="window.__gbXss=1">'
  const taskId = await sendPrompt(page, payload)

  const log = page.getByRole("log", { name: "Chat messages" })
  await expect(log).toContainText(`echo: ${payload}`)
  await expect(log).toContainText("Task succeeded")
  await waitForIdle(page)
  await expect(log.locator("img")).toHaveCount(0)
  expect(
    await page.evaluate(
      () => (window as unknown as { __gbXss?: unknown }).__gbXss,
    ),
  ).toBeUndefined()

  await page.goto(`/#/tasks/${taskId}`)
  const detail = page.getByRole("article", { name: `Task ${taskId}` })
  const state = page.locator('[aria-label="Task state"]')
  await expect(state).toContainText(payload)
  await expect(detail.locator("img")).toHaveCount(0)
  await expect(detail.locator("script")).toHaveCount(0)

  await page.goto("/#/chat")
  await expectNoTokenInUrlLocalStorageOrConsole(
    page,
    E2E_CALLER_TOKEN,
    consoleLines,
  )
})
