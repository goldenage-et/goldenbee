import { useStore } from "zustand"
import { createStore, type StoreApi } from "zustand/vanilla"

/**
 * Saved Gateway addresses.
 *
 * A profile is a local label and URL only. It never stores the caller token,
 * a header, or any Gateway credential. Selecting a profile is a local action
 * that reopens the connection screen; authentication still requires the token.
 */

export const gatewayProfilesStorageKey = "goldenbee.gateway-profiles.v1"

export interface GatewayProfile {
  id: string
  name: string
  url: string
}

export interface GatewayProfilesState {
  profiles: GatewayProfile[]
  addProfile(input: { name?: string; url: string }): GatewayProfile | null
  removeProfile(id: string): void
  renameProfile(id: string, name: string): void
}

export interface GatewayProfilesDeps {
  storage: Storage | null
  key?: string
  createId?: (prefix: string) => string
}

export type GatewayProfilesStore = StoreApi<GatewayProfilesState>

const MAX_NAME = 60

function createId(): string {
  const cryptoApi = globalThis.crypto
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `gateway-${cryptoApi.randomUUID()}`
  }
  return `gateway-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`
}

function cleanName(value: unknown, fallback: string): string {
  if (typeof value !== "string") {
    return fallback
  }
  const trimmed = value.trim().slice(0, MAX_NAME)
  return trimmed.length > 0 ? trimmed : fallback
}

/** Only an http(s) URL is a usable Gateway address. */
export function normalizeProfileUrl(value: string): string | null {
  const trimmed = value.trim()
  if (!trimmed) {
    return null
  }
  try {
    const url = new URL(trimmed)
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      return null
    }
    return url.toString().replace(/\/$/, "")
  } catch {
    return null
  }
}

export function profileNameForUrl(url: string): string {
  try {
    const parsed = new URL(url)
    return parsed.port ? `${parsed.hostname}:${parsed.port}` : parsed.hostname
  } catch {
    return url
  }
}

function sanitize(raw: unknown): GatewayProfile[] {
  if (!Array.isArray(raw)) {
    return []
  }
  const profiles: GatewayProfile[] = []
  const seen = new Set<string>()
  for (const candidate of raw) {
    if (typeof candidate !== "object" || candidate === null) {
      continue
    }
    const record = candidate as Record<string, unknown>
    if (
      typeof record.id !== "string" ||
      record.id === "" ||
      typeof record.url !== "string"
    ) {
      continue
    }
    const url = normalizeProfileUrl(record.url)
    if (!url || seen.has(record.id)) {
      continue
    }
    seen.add(record.id)
    profiles.push({
      id: record.id,
      name: cleanName(record.name, profileNameForUrl(url)),
      url,
    })
  }
  return profiles
}

function load(storage: Storage | null, key: string): GatewayProfile[] {
  if (!storage) {
    return []
  }
  try {
    const raw = storage.getItem(key)
    return sanitize(raw === null ? null : JSON.parse(raw))
  } catch {
    return []
  }
}

function persist(
  storage: Storage | null,
  key: string,
  profiles: GatewayProfile[],
): void {
  if (!storage) {
    return
  }
  try {
    storage.setItem(key, JSON.stringify({ version: 1, profiles }))
  } catch {
    // Profiles remain in memory when storage is unavailable.
  }
}

export function createGatewayProfilesStore(
  deps: GatewayProfilesDeps,
): GatewayProfilesStore {
  const storage = deps.storage
  const key = deps.key ?? gatewayProfilesStorageKey
  const nextId = deps.createId ?? createId
  const initial = load(storage, key)

  const store = createStore<GatewayProfilesState>((set, get) => ({
    profiles: initial,

    addProfile(input) {
      const url = normalizeProfileUrl(input.url)
      if (!url) {
        return null
      }
      const existing = get().profiles.find((profile) => profile.url === url)
      if (existing) {
        return existing
      }
      const profile: GatewayProfile = {
        id: nextId("gateway"),
        name: cleanName(input.name, profileNameForUrl(url)),
        url,
      }
      set({ profiles: [...get().profiles, profile] })
      return profile
    },

    removeProfile(id) {
      set({ profiles: get().profiles.filter((profile) => profile.id !== id) })
    },

    renameProfile(id, name) {
      set({
        profiles: get().profiles.map((profile) =>
          profile.id === id
            ? { ...profile, name: cleanName(name, profileNameForUrl(profile.url)) }
            : profile,
        ),
      })
    },
  }))

  store.subscribe((state) => persist(storage, key, state.profiles))
  return store
}

export const gatewayProfilesStore = createGatewayProfilesStore({
  storage: typeof window === "undefined" ? null : window.localStorage,
})

export function useGatewayProfiles<T>(
  selector: (state: GatewayProfilesState) => T,
  store: GatewayProfilesStore = gatewayProfilesStore,
): T {
  return useStore(store, selector)
}
