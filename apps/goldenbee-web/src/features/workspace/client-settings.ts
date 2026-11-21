import { useStore } from "zustand"
import { createStore, type StoreApi } from "zustand/vanilla"

/**
 * Local Client preferences.
 *
 * These values are presentation-only and never leave the browser. The caller
 * token, provider credentials, and Gateway policy are not part of this store.
 */

export const clientSettingsStorageKey = "goldenbee.client-settings.v1"

export type Density = "comfortable" | "compact"
export type InspectorDefault = "open" | "closed"

export interface ClientSettings {
  density: Density
  inspectorDefault: InspectorDefault
  /** Local Gateway profile id; profiles contain no token. */
  defaultGatewayProfileId: string | null
}

export interface ClientSettingsState extends ClientSettings {
  setDensity(density: Density): void
  setInspectorDefault(value: InspectorDefault): void
  setDefaultGatewayProfile(profileId: string | null): void
}

export interface ClientSettingsDeps {
  storage: Storage | null
  key?: string
}

export type ClientSettingsStore = StoreApi<ClientSettingsState>

const defaults: ClientSettings = {
  density: "comfortable",
  inspectorDefault: "open",
  defaultGatewayProfileId: null,
}

function sanitize(raw: unknown): ClientSettings {
  if (typeof raw !== "object" || raw === null) {
    return { ...defaults }
  }
  const record = raw as Record<string, unknown>
  return {
    density: record.density === "compact" ? "compact" : "comfortable",
    inspectorDefault: record.inspectorDefault === "closed" ? "closed" : "open",
    defaultGatewayProfileId:
      typeof record.defaultGatewayProfileId === "string" &&
      record.defaultGatewayProfileId !== ""
        ? record.defaultGatewayProfileId
        : null,
  }
}

function load(storage: Storage | null, key: string): ClientSettings {
  if (!storage) {
    return { ...defaults }
  }
  try {
    const raw = storage.getItem(key)
    return sanitize(raw === null ? null : JSON.parse(raw))
  } catch {
    return { ...defaults }
  }
}

function persist(
  storage: Storage | null,
  key: string,
  state: ClientSettingsState,
): void {
  if (!storage) {
    return
  }
  const payload: ClientSettings = {
    density: state.density,
    inspectorDefault: state.inspectorDefault,
    defaultGatewayProfileId: state.defaultGatewayProfileId,
  }
  try {
    storage.setItem(key, JSON.stringify(payload))
  } catch {
    // Preferences stay in memory when storage is unavailable.
  }
}

export function createClientSettingsStore(
  deps: ClientSettingsDeps,
): ClientSettingsStore {
  const storage = deps.storage
  const key = deps.key ?? clientSettingsStorageKey
  const initial = load(storage, key)
  const store = createStore<ClientSettingsState>((set) => ({
    ...initial,
    setDensity(density) {
      set({ density })
    },
    setInspectorDefault(inspectorDefault) {
      set({ inspectorDefault })
    },
    setDefaultGatewayProfile(defaultGatewayProfileId) {
      set({ defaultGatewayProfileId })
    },
  }))
  store.subscribe((state) => persist(storage, key, state))
  return store
}

export const clientSettingsStore = createClientSettingsStore({
  storage: typeof window === "undefined" ? null : window.localStorage,
})

export function useClientSettings<T>(
  selector: (state: ClientSettingsState) => T,
  store: ClientSettingsStore = clientSettingsStore,
): T {
  return useStore(store, selector)
}
