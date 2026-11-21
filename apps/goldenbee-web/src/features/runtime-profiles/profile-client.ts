import type {
  CreateProfileRequest,
  DeletedProfile,
  Profile,
  ProfileVersionRequest,
  ReviseProfileRequest,
} from "@goldenbee/sdk"

/**
 * The Worker-owned Runtime Profile administration slice of the Gateway SDK.
 *
 * The Gateway forwards these operations to the owning Worker; the browser never
 * assumes profile state that the Worker did not return.
 */
export interface RuntimeProfileClient {
  profiles(worker: string): Promise<Profile[]>
  profile(worker: string, name: string): Promise<Profile>
  createProfile(worker: string, request: CreateProfileRequest): Promise<Profile>
  reviseProfile(
    worker: string,
    name: string,
    request: ReviseProfileRequest,
  ): Promise<Profile>
  validateProfile(
    worker: string,
    name: string,
    request?: ProfileVersionRequest,
  ): Promise<Profile>
  activateProfile(
    worker: string,
    name: string,
    request?: ProfileVersionRequest,
  ): Promise<Profile>
  deactivateProfile(worker: string, name: string): Promise<Profile>
  deleteProfile(worker: string, name: string): Promise<DeletedProfile>
}
