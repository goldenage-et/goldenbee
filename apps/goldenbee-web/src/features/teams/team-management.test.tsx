import { QueryClient, QueryClientProvider } from "@tanstack/react-query"
import { render, screen, within } from "@testing-library/react"
import userEvent from "@testing-library/user-event"
import { describe, expect, it } from "vitest"

import { TeamDetailPage } from "./team-detail-page"
import { TeamsPage } from "./teams-page"
import {
  createFakeTeamClient,
  scopeDenied,
  teamNotFound,
  teamVersionConflict,
} from "./test-fixtures"

function withQuery(ui: React.ReactNode) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  })
  const user = userEvent.setup()
  render(
    <QueryClientProvider client={queryClient}>
      {ui}
    </QueryClientProvider>,
  )
  return { user, queryClient }
}

async function createTeam(
  user: ReturnType<typeof userEvent.setup>,
  name = "field",
) {
  await user.type(screen.getByLabelText("Team name"), name)
  await user.type(screen.getByLabelText("Master Worker"), "general-a")
  const members = screen.getByRole("group", { name: "Members" })
  await user.type(
    within(members).getByLabelText("Member 1 worker"),
    "general-b",
  )
  await user.type(
    within(members).getByLabelText("Member 1 capabilities"),
    "general",
  )
  await user.click(screen.getByRole("button", { name: "Create team" }))
}

describe("team management", () => {
  it("lists roster identity, version, master, member count, locality, and state", async () => {
    withQuery(<TeamsPage client={createFakeTeamClient()} />)

    expect(screen.getByRole("heading", { name: "Teams" })).toBeInTheDocument()
    const list = await screen.findByRole("table")
    expect(within(list).getByRole("link", { name: "research" })).toHaveAttribute(
      "href",
      "#/teams/research",
    )
    expect(within(list).getByText("4")).toBeInTheDocument()
    expect(within(list).getByText("local")).toBeInTheDocument()
    expect(within(list).getByText("2 members")).toBeInTheDocument()
    expect(within(list).getByText("Active")).toBeInTheDocument()
  })

  it("validates required fields before sending a team request", async () => {
    const client = createFakeTeamClient()
    const { user } = withQuery(<TeamsPage client={client} />)
    await screen.findByRole("table")

    await user.click(screen.getByRole("button", { name: "Create team" }))

    expect(client.calls.create).toHaveLength(0)
    expect(screen.getByText("Enter a team name.")).toBeInTheDocument()
    expect(screen.getByText("Designate a Master Worker.")).toBeInTheDocument()
  })

  it("creates an inactive roster and shows it without local guesswork", async () => {
    const client = createFakeTeamClient()
    const { user } = withQuery(<TeamsPage client={client} />)
    await screen.findByRole("table")

    await createTeam(user)

    expect(await screen.findByRole("link", { name: "field" })).toBeInTheDocument()
    expect(client.calls.create).toHaveLength(1)
    expect(client.calls.create[0]).toEqual({
      name: "field",
      master: "general-a",
      members: [{ worker: "general-b", capabilities: ["general"] }],
    })
    expect(await screen.findByText("Version 1 created")).toBeInTheDocument()
  })

  it("validates and activates the observed roster version", async () => {
    const client = createFakeTeamClient()
    const { user } = withQuery(
      <TeamDetailPage client={client} name="research" />,
    )

    expect(
      await screen.findByRole("heading", { name: "research" }),
    ).toBeInTheDocument()
    expect(screen.getByText("Version 4")).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Validate team" }))
    expect(
      await screen.findByText("Validation passed for version 4."),
    ).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Deactivate team" }))
    expect(
      await screen.findByText("Team deactivated at version 4."),
    ).toBeInTheDocument()
    expect(screen.getByText("Inactive")).toBeInTheDocument()

    await user.click(screen.getByRole("button", { name: "Activate team" }))
    expect(
      await screen.findByText("Team activated at version 4."),
    ).toBeInTheDocument()
    expect(screen.getByText("Active")).toBeInTheDocument()
    expect(client.calls.validate).toEqual([{ name: "research", version: 4 }])
    expect(client.calls.deactivate).toEqual([{ name: "research", version: 4 }])
    expect(client.calls.activate).toEqual([{ name: "research", version: 4 }])
  })

  it("revises the roster with the expected version and shows the new version", async () => {
    const client = createFakeTeamClient()
    const { user } = withQuery(
      <TeamDetailPage client={client} name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })

    const master = screen.getByLabelText("Master Worker")
    await user.clear(master)
    await user.type(master, "general-b")
    await user.click(screen.getByRole("button", { name: "Save revision" }))

    expect(await screen.findByText("Roster revised to version 5.")).toBeInTheDocument()
    expect(client.calls.update[0]).toMatchObject({
      name: "research",
      request: {
        expected_version: 4,
        master: "general-b",
      },
    })
    // The previous Master row is carried forward, so its narrowed capability
    // set is not silently widened by re-inference.
    expect(client.calls.update[0].request.members).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          worker: "general-a",
          capabilities: ["general"],
        }),
        expect.objectContaining({
          worker: "research-b",
          capabilities: ["research"],
        }),
      ]),
    )
  })

  it("shows a stale roster without pretending the revision was saved", async () => {
    const client = createFakeTeamClient()
    client.updateTeam = async (name, request) => {
      client.calls.update.push({ name, request })
      throw teamVersionConflict()
    }
    const { user } = withQuery(
      <TeamDetailPage client={client} name="research" />,
    )
    await screen.findByRole("heading", { name: "research" })

    await user.click(screen.getByRole("button", { name: "Save revision" }))

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Team changed elsewhere")
    expect(alert).toHaveTextContent("team.version_conflict")
    expect(screen.getByText("Version 4")).toBeInTheDocument()
    expect(screen.queryByText("Version 5")).not.toBeInTheDocument()
  })

  it("keeps a denied create visible and does not add a local row", async () => {
    const client = createFakeTeamClient()
    client.createTeam = async (request) => {
      client.calls.create.push(request)
      throw scopeDenied()
    }
    const { user } = withQuery(<TeamsPage client={client} />)
    await screen.findByRole("table")

    await createTeam(user)

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.team_write")
    expect(screen.queryByRole("link", { name: "field" })).not.toBeInTheDocument()
    expect(
      screen.getByRole("button", { name: "Create team" }),
    ).toBeEnabled()
  })

  it("renders a denied roster read as an unauthorized state", async () => {
    const client = createFakeTeamClient()
    client.teams = async () => {
      throw scopeDenied()
    }
    withQuery(<TeamsPage client={client} />)

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Not permitted")
    expect(alert).toHaveTextContent("scope.team_write")
    expect(screen.getByRole("heading", { name: "Teams" })).toBeInTheDocument()
  })

  it("distinguishes loading, empty, and error states", async () => {
    const client = createFakeTeamClient([])
    withQuery(<TeamsPage client={client} />)

    expect(screen.getByText("Loading teams.")).toBeInTheDocument()
    expect(await screen.findByText("No teams configured.")).toBeInTheDocument()
  })

  it("surfaces a missing roster as a typed detail error", async () => {
    const client = createFakeTeamClient()
    client.team = async () => {
      throw teamNotFound()
    }
    withQuery(<TeamDetailPage client={client} name="missing" />)

    const alert = await screen.findByRole("alert")
    expect(alert).toHaveTextContent("Team not found")
    expect(alert).toHaveTextContent("team.not_found")
  })
})
