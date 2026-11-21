import { expect, type Page } from "@playwright/test"

export const WORKER = "worker-general"
export const AGENT = "general"
export const EFFECT_WORKER = "worker-effect"
export const EFFECT_AGENT = "effect"

export function trackConsole(page: Page): string[] {
  const lines: string[] = []
  page.on("console", (message) => {
    lines.push(message.text())
  })
  page.on("pageerror", (error) => {
    lines.push(error.message)
  })
  return lines
}

/** Connect through the visible form, exactly as an operator does. */
export async function connect(
  page: Page,
  token: string,
  options: { remember?: boolean; expected?: string } = {},
): Promise<void> {
  await page.goto("/")
  await expect(
    page.getByRole("heading", { name: "Connect to your Gateway" }),
  ).toBeVisible()
  await page.getByLabel("Caller token").fill(token)
  if (options.remember) {
    await page.getByRole("checkbox", { name: "Remember token for this tab" }).check()
  }
  await page.getByRole("button", { name: "Connect" }).click()
  await expect(
    page.getByText(options.expected ?? "Gateway connected"),
  ).toBeVisible()
}

/** Open the chat route and wait for the selection controls to be populated. */
export async function openChat(page: Page): Promise<void> {
  await page.goto("/#/chat")
  await expect(page.getByRole("textbox", { name: "Message" })).toBeVisible()
  await expect(page.getByLabel("Worker")).toBeVisible()
}

export async function selectTarget(
  page: Page,
  worker = WORKER,
  agent = AGENT,
): Promise<void> {
  await expect
    .poll(async () => page.getByLabel("Worker").locator("option").count())
    .toBeGreaterThan(0)
  await page.getByLabel("Worker").selectOption(worker)
  await page.getByLabel("Agent").selectOption(agent)
  await expect(page.getByLabel("Worker")).toHaveValue(worker)
  await expect(page.getByLabel("Agent")).toHaveValue(agent)
  await expect(page.getByLabel("Runtime Profile", { exact: true })).toBeVisible()
}

/**
 * Send one prompt and return the real Gateway task id from the submit response.
 *
 * The response is the public protocol boundary the UI uses, not an internal
 * component value.
 */
export async function sendPrompt(page: Page, text: string): Promise<string> {
  await page.getByRole("textbox", { name: "Message" }).fill(text)
  await expect(page.getByRole("button", { name: "Send message" })).toBeEnabled()
  const responsePromise = page.waitForResponse((response) => {
    const url = new URL(response.url())
    return (
      response.request().method() === "POST" && url.pathname === "/v1/tasks"
    )
  })
  await page.getByRole("button", { name: "Send message" }).click()
  const response = await responsePromise
  expect([200, 202]).toContain(response.status())
  const body = (await response.json()) as { task_id?: string }
  expect(body.task_id).toBeTruthy()
  return body.task_id!
}

export async function expectNoTokenInUrlLocalStorageOrConsole(
  page: Page,
  token: string,
  consoleLines: string[],
): Promise<void> {
  expect(page.url()).not.toContain(token)
  const localStorageValues = await page.evaluate(() => {
    const values: Record<string, string> = {}
    for (let index = 0; index < localStorage.length; index += 1) {
      const key = localStorage.key(index)
      if (key !== null) {
        values[key] = localStorage.getItem(key) ?? ""
      }
    }
    return values
  })
  expect(JSON.stringify(localStorageValues)).not.toContain(token)
  expect(consoleLines.join("\n")).not.toContain(token)
}

export async function sessionStorageToken(page: Page): Promise<string | null> {
  return page.evaluate(() => sessionStorage.getItem("goldenbee.caller-token"))
}

/** Wait until the chat is idle again, i.e. the active task reached a terminal state. */
export async function waitForIdle(page: Page): Promise<void> {
  await expect(page.getByRole("textbox", { name: "Message" })).toBeEnabled()
}
