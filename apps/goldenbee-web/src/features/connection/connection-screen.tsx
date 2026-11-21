import { useState, type FormEvent } from "react"
import { AlertCircleIcon, KeyRoundIcon, LoaderCircleIcon, LockKeyholeIcon, ServerIcon } from "lucide-react"

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Checkbox } from "@/components/ui/checkbox"
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { connectionErrorCopy as errorCopy } from "@/features/connection/connection-errors"
import {
  connectionStore,
  useConnection,
  type ConnectionStore,
} from "@/features/connection/connection-store"

export function ConnectionScreen({
  store = connectionStore,
}: {
  store?: ConnectionStore
}) {
  const status = useConnection((state) => state.status, store)
  const initialUrl = useConnection((state) => state.gatewayUrl, store)
  const initialToken = useConnection((state) => state.token, store)
  const initialRemember = useConnection((state) => state.rememberToken, store)
  const error = useConnection((state) => state.error, store)
  const connect = useConnection((state) => state.connect, store)

  const [gatewayUrl, setGatewayUrl] = useState(initialUrl)
  const [token, setToken] = useState(initialToken)
  const [rememberToken, setRememberToken] = useState(initialRemember)
  const connecting = status === "connecting"
  const invalid = status === "invalid"

  function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (connecting) {
      return
    }
    void connect({ gatewayUrl, token, rememberToken })
  }

  return (
    <div className="grid flex-1 items-start gap-8 lg:grid-cols-[minmax(0,1fr)_minmax(22rem,26rem)] lg:gap-14">
      <section className="max-w-xl lg:pt-6">
        <p className="text-xs font-medium tracking-[0.16em] text-muted-foreground uppercase">
          Operator connection
        </p>
        <h1 className="mt-3 font-heading text-3xl font-semibold tracking-tight text-balance sm:text-4xl">
          Connect to your Gateway
        </h1>
        <p className="mt-4 text-sm leading-6 text-muted-foreground sm:text-base">
          Use the same Gateway address and caller token as your other Goldenbee
          Clients. The browser talks to the Gateway v1 API through a typed
          client, with no BFF and no server-side session.
        </p>

        <ul className="mt-8 grid gap-4 text-sm">
          <li className="flex gap-3">
            <ServerIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <span>
              <span className="font-medium">Verify the deployment.</span> Gateway
              health and caller-scoped Worker discovery confirm the connection.
            </span>
          </li>
          <li className="flex gap-3">
            <LockKeyholeIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <span>
              <span className="font-medium">Keep the token bounded.</span> Memory
              is the default. Current-tab storage is an explicit choice, and
              localStorage is never used.
            </span>
          </li>
          <li className="flex gap-3">
            <KeyRoundIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <span>
              <span className="font-medium">Stay behind Gateway policy.</span>{" "}
              Worker routing, approvals, and provider credentials remain
              Gateway-owned.
            </span>
          </li>
        </ul>
      </section>

      <Card className="w-full">
        <CardHeader>
          <CardTitle>Gateway connection</CardTitle>
          <CardDescription>
            Enter the operator-facing address and a caller token.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={onSubmit} noValidate>
            <FieldGroup>
              <Field
                data-invalid={
                  invalid && error?.field === "gatewayUrl" ? true : undefined
                }
              >
                <FieldLabel htmlFor="gateway-url">Gateway URL</FieldLabel>
                <Input
                  id="gateway-url"
                  name="gatewayUrl"
                  type="url"
                  inputMode="url"
                  autoComplete="url"
                  spellCheck={false}
                  placeholder="http://127.0.0.1:8080"
                  value={gatewayUrl}
                  onChange={(event) => setGatewayUrl(event.target.value)}
                  disabled={connecting}
                  aria-invalid={
                    invalid && error?.field === "gatewayUrl" ? true : undefined
                  }
                />
                <FieldDescription>
                  Development uses the same-origin Vite proxy at /v1.
                </FieldDescription>
                {invalid && error?.field === "gatewayUrl" ? (
                  <FieldError>{error.message}</FieldError>
                ) : null}
              </Field>

              <Field
                data-invalid={
                  invalid && error?.field === "token" ? true : undefined
                }
              >
                <FieldLabel htmlFor="caller-token">Caller token</FieldLabel>
                <Input
                  id="caller-token"
                  name="callerToken"
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="Bearer token from Gateway configuration"
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                  disabled={connecting}
                  aria-invalid={
                    invalid && error?.field === "token" ? true : undefined
                  }
                />
                <FieldDescription>
                  Sent only in the Authorization header. It never appears in a
                  URL.
                </FieldDescription>
                {invalid && error?.field === "token" ? (
                  <FieldError>{error.message}</FieldError>
                ) : null}
              </Field>

              <Field orientation="horizontal">
                <Checkbox
                  aria-label="Remember token for this tab"
                  checked={rememberToken}
                  onCheckedChange={(checked) =>
                    setRememberToken(checked === true)
                  }
                  disabled={connecting}
                />
                <div className="grid gap-1">
                  <span className="text-sm font-medium">
                    Remember token for this tab
                  </span>
                  <span className="text-xs text-muted-foreground">
                    Uses sessionStorage. Closing the tab clears it.
                  </span>
                </div>
              </Field>

              {invalid && error && !error.field ? (
                <FieldError>{error.message}</FieldError>
              ) : null}

              {!invalid && error ? (
                <Alert variant="destructive">
                  <AlertCircleIcon aria-hidden="true" />
                  <AlertTitle>{errorCopy(error).title}</AlertTitle>
                  <AlertDescription>
                    <p>{errorCopy(error).description}</p>
                    <p>{error.message}</p>
                    {error.recovery ? (
                      <p className="mt-1">
                        <span className="font-medium text-foreground">Next: </span>
                        {error.recovery}
                      </p>
                    ) : null}
                  </AlertDescription>
                </Alert>
              ) : null}

              <div className="grid gap-2">
                <Button
                  type="submit"
                  size="lg"
                  className="w-full"
                  disabled={connecting}
                  aria-busy={connecting}
                >
                  {connecting ? (
                    <LoaderCircleIcon
                      data-icon="inline-start"
                      className="animate-spin"
                      aria-hidden="true"
                    />
                  ) : null}
                  {connecting ? "Connecting..." : "Connect"}
                </Button>
                {connecting ? (
                  <p className="text-center text-xs text-muted-foreground" aria-live="polite">
                    Checking Gateway health and caller token.
                  </p>
                ) : null}
              </div>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
    </div>
  )
}
