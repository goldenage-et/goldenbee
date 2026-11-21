import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import type { WorkerSummary } from "@goldenbee/sdk"

import { ProfileDetailPage } from "./profile-detail-page"
import { ProfilesPage } from "./profiles-page"
import {
  createFakeProfileClient,
  profileFixture,
  profileNotFound,
  revisionConflict,
  scopeDenied,
} from "./test-fixtures"

const workers: WorkerSummary[] = [
  {
    id: "general-a",
    instance: "default",
    package: "general",
    version: "0.1.0",
    status: "approved",
    reachable: true,
    locality: "local",
  },
  {
    id: "research-b",
    instance: "research",
    package: "general",
    version: "0.1.0",
    status: "approved",
    reachable: true,
    locality: "local",
  },
]

function withQuery(ui: React.ReactNode) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>,
  )
  return { user, queryClient }
}

async function fillProfileForm(user: ReturnType<typeof userEvent.setup>) {
  await user.type(screen.getByLabelText("Profile name"), "field")
  await user.type(
    screen.getByLabelText("Instruction"),
    "Answer with a short brief.",
  )
  await user.type(screen.getByLabelText("Model"), "openai/gpt-4.1")
}

describe("runtime profile management", () => {
  it("lists Worker-owned profiles with revision, model, composition, and counts", async () => {
    withQuery(
      <ProfilesPage client={createFakeProfileClient()} workers={workers} />,
    )

    expect(
      screen.getByRole("heading", { name: "Runtime Profiles" }),
    ).toBeInTheDocument()
    const list = await screen.findByRole("table")
    expect(within(list).getByRole("link", { name: "research" })).toHaveAttribute(
      "href",
      "#/profiles/general-a/research",
    )
    expect(within(list).getByText("2")).toBeInTheDocument()
    expect(within(list).getByText("openai/gpt-4.1")).toBeInTheDocument()
    expect(within(list).getByText("sequential")).toBeInTheDocument()
    expect(within(list).getByText("1 skill")).toBeInTheDocument()
    expect(within(list).getByText("1 MCP server")).toBeInTheDocument()
  })

  it("switches the profile catalog to the selected Worker", async () => {
    const client = createFakeProfileClient({
      "general-a": [],
      "research-b": [],
    })
    const { user } = withQuery(<ProfilesPage client={client} workers={workers} />)
    await screen.findByText("No profiles on this Worker.")

    await user.selectOptions(screen.getByLabelText("Worker"), "research-b")

    expect(window.location.hash).toBe("#/profiles/research-b")
    expect(client.calls.profiles).toContain("research-b")
  })

  it("validates required fields before sending a create request", async () => {
    const client = createFakeProfileClient()
    const { user } = withQuery(<ProfilesPage client={client} workers={workers} />)
    await screen.findByRole("table")

    await user.click(screen.getByRole("button", { name: "New profile" }))
    const dialog = await screen.findByRole("dialog")
    await user.click(
      within(dialog).getByRole("button", { name: "Create profile" }),
    )

    expect(client.calls.create).toHaveLength(0)
    expect(within(dialog).getByText("Enter a profile name.")).toBeInTheDocument()
    expect(
      within(dialog).getByText("Enter an agent instruction."),
    ).toBeInTheDocument()
    expect(within(dialog).getByText("Enter a model.")).toBeInTheDocument()
  })

  it("creates an inactive revision and shows it after the Worker confirms", async () => {
    const client = createFakeProfileClient()
    const { user } = withQuery(<ProfilesPage client={client} workers={workers} />)
    await screen.findByRole("table")

    await user.click(screen.getByRole("button", { name: "New profile" }))
    await fillProfileForm(user)
    await user.click(
      within(await screen.findByRole("dialog")).getByRole("button", {
        name: "Create profile",
      }),
    )

    expect(await screen.findByRole("link", { name: "field" })).toBeInTheDocument()
    expect(client.calls.create).toHaveLength(1)
    expect(client.calls.create[0]).toMatchObject({
      worker: "general-a",
      request: {
        name: "field",
        revision: {
          revision: 0,
          instruction: "Answer with a short brief.",
          model: "openai/gpt-4.1",
        },
      },
    })
  })

  it("revises a profile with the observed revision count", async () => {
    const client = createFakeProfileClient()
    const { user } = withQuery(
      <ProfileDetailPage client={client} worker="general-a" name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })
    expect(screen.getByText("Active revision 2")).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Save revision" }))

    expect(await screen.findByText("Revision 3 created.")).toBeInTheDocument()
    expect(client.calls.revise[0]).toMatchObject({
      worker: "general-a",
      name: "research",
      request: { expected_revision: 2 },
    })
  })

  it("reports a stale revision without claiming the revision was saved", async () => {
    const client = createFakeProfileClient()
    client.reviseProfile = async (worker, name, request) => {
      client.calls.revise.push({ worker, name, request })
      throw revisionConflict(3, 2)
    }
    const { user } = withQuery(
      <ProfileDetailPage client={client} worker="general-a" name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })

    await user.click(screen.getByRole("button", { name: "Save revision" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Profile changed elsewhere")
    expect(alert).toHaveTextContent("profile.revision_conflict")
    expect(screen.getByText("Active revision 2")).toBeInTheDocument()
    expect(screen.queryByText("Revision 3 created.")).not.toBeInTheDocument()
  })

  it("validates, activates, deactivates, and deletes an inactive profile", async () => {
    const client = createFakeProfileClient()
    const { user } = withQuery(
      <ProfileDetailPage client={client} worker="general-a" name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })

    await user.click(screen.getByRole("button", { name: "Validate revision" }))
    expect(await screen.findByText("Validated revision 2.")).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Deactivate profile" }))
    expect(
      await screen.findByText("Deactivated. No revision is active."),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Activate revision" }))
    expect(await screen.findByText("Activated revision 2.")).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Deactivate profile" }))
    await screen.findByText("Deactivated. No revision is active.")

    await user.click(screen.getByRole("button", { name: "Delete profile" }))
    const dialog = await screen.findByRole("dialog")
    await user.click(
      within(dialog).getByRole("button", { name: "Delete permanently" }),
    )

    expect(await screen.findByText("Profile research deleted.")).toBeInTheDocument()
    expect(client.calls.delete).toEqual([
      { worker: "general-a", name: "research" },
    ])
    expect(client.calls.validate[0]).toMatchObject({
      worker: "general-a",
      name: "research",
      revision: 2,
    })
  })

  it("keeps a denied create visible and does not add a local row", async () => {
    const client = createFakeProfileClient()
    client.createProfile = async (worker, request) => {
      client.calls.create.push({ worker, request })
      throw scopeDenied()
    }
    const { user } = withQuery(<ProfilesPage client={client} workers={workers} />)
    await screen.findByRole("table")

    await user.click(screen.getByRole("button", { name: "New profile" }))
    await fillProfileForm(user)
    const dialog = await screen.findByRole("dialog")
    await user.click(
      within(dialog).getByRole("button", { name: "Create profile" }),
    )

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.profile_write")
    expect(screen.queryByRole("link", { name: "field" })).not.toBeInTheDocument()
    expect(
      within(dialog).getByRole("button", { name: "Create profile" }),
    ).toBeEnabled()
  })

  it("shows an unauthorized catalog read without hiding the Worker selector", async () => {
    const client = createFakeProfileClient()
    client.profiles = async () => {
      throw scopeDenied()
    }
    withQuery(<ProfilesPage client={client} workers={workers} />)

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(screen.getByLabelText("Worker")).toBeInTheDocument()
  })

  it("distinguishes loading, empty, and no-worker states", async () => {
    const client = createFakeProfileClient({ "general-a": [] })
    const first = withQuery(
      <ProfilesPage client={client} workers={workers} />,
    )
    expect(screen.getByText("Loading profiles.")).toBeInTheDocument()
    expect(
      await screen.findByText("No profiles on this Worker."),
    ).toBeInTheDocument()
    first.queryClient.clear()

    withQuery(<ProfilesPage client={client} workers={[]} />)
    expect(
      screen.getByText("No Worker is visible to this caller."),
    ).toBeInTheDocument()
  })

  it("keeps a denied activation visible and leaves the revision inactive", async () => {
    const client = createFakeProfileClient({
      "general-a": [profileFixture({ active_revision: 0 })],
    })
    client.activateProfile = async (worker, name, request) => {
      client.calls.activate.push({ worker, name, revision: request?.revision })
      throw scopeDenied()
    }
    const { user } = withQuery(
      <ProfileDetailPage client={client} worker="general-a" name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })
    expect(screen.getByText("No revision is active")).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Activate revision" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.profile_write")
    expect(screen.getByText("No revision is active")).toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Activate revision" }),
    ).toBeEnabled()
  })

  it("renders the instruction and secret references as safe metadata", async () => {
    withQuery(
      <ProfileDetailPage client={createFakeProfileClient()} worker="general-a" name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })

    expect(
      screen.getAllByText(
        "Research the question and cite the strongest evidence.",
      ).length,
    ).toBeGreaterThan(0)
    expect(screen.getByText("env:SEARCH_KEY")).toBeInTheDocument()
    expect(
      screen.getByText(/never return credential values/i),
    ).toBeInTheDocument()
    expect(document.body.textContent ?? "").not.toContain("sk-")
    expect(window.localStorage.length).toBe(0)
  })

  it("surfaces a missing profile as a typed detail error", async () => {
    const client = createFakeProfileClient()
    client.profile = async () => {
      throw profileNotFound("missing")
    }
    withQuery(
      <ProfileDetailPage client={client} worker="general-a" name="missing" />,
    )

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Profile not found")
    expect(alert).toHaveTextContent("profile.not_found")
  })
})
