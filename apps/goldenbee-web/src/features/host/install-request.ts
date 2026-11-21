import type { PackageInstallRequest } from "@goldenbee/sdk"

export type InstallSourceKind = "url" | "folder"

export interface InstallFormValues {
  kind: InstallSourceKind
  url: string
  path: string
  packageName: string
  expectedDigest: string
}

export interface InstallBuildResult {
  request?: PackageInstallRequest
  error?: { field: "url" | "path"; message: string }
}

/**
 * Turn the install form into the Gateway request.
 *
 * A URL is the normal path. A folder is a Gateway-host path, never a browser
 * upload, so only an explicit operator-entered absolute path is accepted.
 */
export function buildInstallRequest(
  values: InstallFormValues,
): InstallBuildResult {
  if (values.kind === "url") {
    const url = values.url.trim()
    if (!url) {
      return { error: { field: "url", message: "Enter a package URL." } }
    }
    let parsed: URL
    try {
      parsed = new URL(url)
    } catch {
      return {
        error: { field: "url", message: "Enter an absolute http:// or https:// URL." },
      }
    }
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return {
        error: { field: "url", message: "Enter an absolute http:// or https:// URL." },
      }
    }
    return { request: withOptionalFields({ source: { kind: "url", url } }, values) }
  }

  const path = values.path.trim()
  if (!path) {
    return {
      error: { field: "path", message: "Enter a path on the Gateway host." },
    }
  }
  if (!path.startsWith("/") || path.split("/").includes("..")) {
    return {
      error: {
        field: "path",
        message: "Enter an absolute Gateway-host path without '..' segments.",
      },
    }
  }
  return { request: withOptionalFields({ source: { kind: "folder", path } }, values) }
}

function withOptionalFields(
  request: PackageInstallRequest,
  values: InstallFormValues,
): PackageInstallRequest {
  const packageName = values.packageName.trim()
  const expectedDigest = values.expectedDigest.trim()
  return {
    ...request,
    ...(packageName ? { package: packageName } : {}),
    ...(expectedDigest ? { expected_digest: expectedDigest } : {}),
  }
}
