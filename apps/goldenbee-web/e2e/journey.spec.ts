import { expect, test } from "@playwright/test"

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
import { E2E_CALLER_TOKEN } from "./tokens"

test("recovers an active task after a browser reload from the durable cursor", async ({
  page,
}) => {
  await connect(page, E2E_CALLER_TOKEN, { remember: true })
  await openChat(page)
  await selectTarget(page, WORKER, AGENT)

  let streamRequests = 0
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.endsWith("/events/stream")) {
      streamRequests += 1
    }
  })

  // The deterministic model holds this response so the task is still running
  // when the browser drops its stream.
  await sendPrompt(page, "delay:5000 reload recovery probe")
  const log = page.getByRole("log", { name: "Chat messages" })
  await expect(log).toContainText("Task running")

  await page.reload()
  await expect(
    page.getByRole("heading", { name: "Connect to your Gateway" }),
  ).toBeVisible()
  await page.getByRole("button", { name: "Connect" }).click()
  await expect(page.getByLabel("Worker")).toBeVisible()

  // The task keeps running on the Worker; the restored chat opens a new stream
  // from the saved durable cursor and the terminal projection supplies the full
  // output.
  const restoredLog = page.getByRole("log", { name: "Chat messages" })
  await expect(restoredLog).toContainText(
    "echo: delay:5000 reload recovery probe",
    { timeout: 30_000 },
  )
  await expect(restoredLog).toContainText("Task succeeded")
  await expect.poll(() => streamRequests).toBeGreaterThanOrEqual(2)
  await waitForIdle(page)
})

test("connect, stream an answer and tool, inspect durable evidence, reload and restore", async ({
  page,
}) => {
  const consoleLines = trackConsole(page)

  await connect(page, E2E_CALLER_TOKEN, { remember: true })
  await openChat(page)
  await selectTarget(page)

  const log = page.getByRole("log", { name: "Chat messages" })

  const firstTask = await sendPrompt(page, "hello goldenbee e2e")
  await expect(log).toContainText("echo: hello goldenbee e2e")
  await expect(log).toContainText("Task succeeded")

  // The deterministic fixture model calls the real native artifact tool, and
  // the runtime emits durable `tool.call` / `tool.result` events.
  const toolTask = await sendPrompt(
    page,
    'call:save_artifact {"name":"e2e.txt","text":"streamed"}',
  )
  await expect(log).toContainText("Ran save_artifact")
  await expect(log).toContainText("Task succeeded")
  await waitForIdle(page)

  // Open the focused-session inspector and follow its task link. This is the
  // same path an operator uses; it does not reach into component internals.
  const inspector = page.getByLabel("Focused session inspector")
  if (!(await inspector.isVisible())) {
    await page.getByRole("button", { name: /session inspector/ }).click()
  }
  await expect(inspector).toBeVisible()
  await expect(inspector).toContainText(toolTask)
  await inspector.getByRole("link", { name: toolTask }).first().click()

  const detail = page.getByRole("article", { name: `Task ${toolTask}` })
  await expect(detail).toBeVisible()
  await expect(detail).toContainText(toolTask)
  const state = page.locator('[aria-label="Task state"]')
  await expect(state).toContainText("succeeded")
  await expect(state).toContainText("echo: tool result")

  const events = page.locator('[aria-label="Retained events"]')
  await expect(events).toContainText("tool.call")
  await expect(events).toContainText("tool.result")
  await expect(events).toContainText("task.succeeded")

  // Reload while on the chat route. The token is restored only because the
  // operator explicitly chose "remember for this tab"; IndexedDB restores the
  // local chat and its messages.
  await page.goto("/#/chat")
  await page.reload()
  await expect(
    page.getByRole("heading", { name: "Connect to your Gateway" }),
  ).toBeVisible()
  await page.getByRole("button", { name: "Connect" }).click()
  await expect(page.getByLabel("Worker")).toBeVisible()
  await openChat(page)
  const restoredLog = page.getByRole("log", { name: "Chat messages" })
  await expect(restoredLog).toContainText("hello goldenbee e2e")
  await expect(restoredLog).toContainText("Ran save_artifact")

  await expectNoTokenInUrlLocalStorageOrConsole(
    page,
    E2E_CALLER_TOKEN,
    consoleLines,
  )
  // The only storage that may hold the token is this tab's sessionStorage.
  expect(await sessionStorageToken(page)).toBe(E2E_CALLER_TOKEN)

  // The first task is still readable by id and its evidence did not depend on
  // the closed stream.
  await page.goto(`/#/tasks/${firstTask}`)
  await expect(page.getByRole("article", { name: `Task ${firstTask}` })).toBeVisible()
  await expect(page.locator('[aria-label="Retained events"]')).toContainText(
    "task.succeeded",
  )
})
