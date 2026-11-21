import { readFile } from "node:fs/promises"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"

import { expect, test, type APIRequestContext } from "@playwright/test"

import {
  connect,
  EFFECT_AGENT,
  EFFECT_WORKER,
  expectNoTokenInUrlLocalStorageOrConsole,
  openChat,
  selectTarget,
  sendPrompt,
  trackConsole,
  waitForIdle,
} from "./support"
import { E2E_BASE_URL, E2E_CALLER_TOKEN } from "./tokens"

interface ApprovalRecord {
  id: string
  task_id: string
  operation: string
  target: string
  digest: string
  risk: string
  status: string
  receipt?: string
}

interface ApprovalEnvelope {
  task_id: string
  worker: string
  approval: ApprovalRecord
}

const here = dirname(fileURLToPath(import.meta.url))
const effectLedger = join(here, ".stack", "effect", "external", "effects.log")

async function approvalsFor(
  request: APIRequestContext,
  taskId: string,
): Promise<ApprovalEnvelope[]> {
  const response = await request.get(
    `${E2E_BASE_URL}/v1/tasks/${taskId}/approvals?pending=false`,
    { headers: { Authorization: `Bearer ${E2E_CALLER_TOKEN}` } },
  )
  expect(response.status()).toBe(200)
  return (await response.json()) as ApprovalEnvelope[]
}

async function pendingApproval(
  request: APIRequestContext,
  taskId: string,
): Promise<ApprovalEnvelope> {
  let found: ApprovalEnvelope | undefined
  await expect
    .poll(async () => {
      const approvals = await approvalsFor(request, taskId)
      found = approvals.find((entry) => entry.approval.status === "pending")
      return found !== undefined
    })
    .toBe(true)
  return found!
}

async function ledgerText(): Promise<string> {
  try {
    return await readFile(effectLedger, "utf8")
  } catch {
    return ""
  }
}

test("approval card shows the exact digest, approves one effect, rejects another, and records both", async ({
  page,
  request,
}) => {
  const consoleLines = trackConsole(page)

  await connect(page, E2E_CALLER_TOKEN)
  await openChat(page)
  await selectTarget(page, EFFECT_WORKER, EFFECT_AGENT)

  const approvedTask = await sendPrompt(
    page,
    'call:ledger_write {"entry":"e2e-approval-target"}',
  )

  const firstCard = page.locator('[aria-label="Approval ledger_write"]').first()
  await expect(firstCard).toBeVisible()
  await expect(firstCard).toContainText("e2e-approval-target")
  await expect(firstCard).toContainText("external-write")

  const pending = await pendingApproval(request, approvedTask)
  expect(pending.approval.operation).toBe("ledger_write")
  expect(pending.approval.target).toBe("e2e-approval-target")
  expect(pending.approval.digest).toMatch(/^sha256:/)

  const shownDigest = (
    await firstCard.locator("dd").filter({ hasText: /^sha256:/ }).first().textContent()
  )?.trim()
  expect(shownDigest).toBe(pending.approval.digest)

  await firstCard.getByRole("button", { name: "Approve" }).click()
  await expect(firstCard).toContainText("approved")
  await expect(page.getByRole("log", { name: "Chat messages" })).toContainText(
    "Task succeeded",
  )
  await waitForIdle(page)

  const approved = (await approvalsFor(request, approvedTask)).find(
    (entry) => entry.approval.id === pending.approval.id,
  )
  expect(approved?.approval.status).toBe("approved")
  expect(approved?.approval.digest).toBe(pending.approval.digest)
  expect(approved?.approval.receipt).toBeTruthy()

  // The real external ledger proves the approved effect happened exactly once.
  const afterApprove = await ledgerText()
  expect(afterApprove).toContain("e2e-approval-target")

  await firstCard.getByRole("link", { name: "Open task" }).click()
  const detail = page.getByRole("article", { name: `Task ${approvedTask}` })
  await expect(detail).toBeVisible()
  const approvalsSection = page.locator('[aria-label="Task approvals"]')
  await expect(approvalsSection).toContainText("approved")
  const events = page.locator('[aria-label="Retained events"]')
  await expect(events).toContainText("approval.approved")
  await expect(events).toContainText("operation.committed")

  // A second protected effect is rejected. The card and the task record both
  // keep the rejection, and the external ledger never receives the target.
  await page.goto("/#/chat")
  await selectTarget(page, EFFECT_WORKER, EFFECT_AGENT)
  const rejectedTask = await sendPrompt(
    page,
    'call:ledger_write {"entry":"e2e-reject-target"}',
  )
  await expect(
    page.locator('[aria-label="Approval ledger_write"]'),
  ).toHaveCount(2)
  const secondCard = page.locator('[aria-label="Approval ledger_write"]').last()
  await expect(secondCard).toContainText("e2e-reject-target")
  const secondPending = await pendingApproval(request, rejectedTask)
  const secondShownDigest = (
    await secondCard.locator("dd").filter({ hasText: /^sha256:/ }).first().textContent()
  )?.trim()
  expect(secondShownDigest).toBe(secondPending.approval.digest)

  await secondCard.getByRole("button", { name: "Reject" }).click()
  await expect(secondCard).toContainText("rejected")
  await waitForIdle(page)
  const rejected = (await approvalsFor(request, rejectedTask)).find(
    (entry) => entry.approval.id === secondPending.approval.id,
  )
  expect(rejected?.approval.status).toBe("rejected")
  expect(rejected?.approval.digest).toBe(secondPending.approval.digest)

  await secondCard.getByRole("link", { name: "Open task" }).click()
  await expect(
    page.getByRole("article", { name: `Task ${rejectedTask}` }),
  ).toBeVisible()
  await expect(page.locator('[aria-label="Task approvals"]')).toContainText(
    "rejected",
  )
  const afterReject = await ledgerText()
  expect(afterReject).toContain("e2e-approval-target")
  expect(afterReject).not.toContain("e2e-reject-target")

  await expectNoTokenInUrlLocalStorageOrConsole(
    page,
    E2E_CALLER_TOKEN,
    consoleLines,
  )
})
