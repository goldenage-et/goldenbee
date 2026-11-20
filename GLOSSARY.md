# Goldenbee

Goldenbee has three building blocks: Worker, Gateway, and Client. Shared libraries support those building blocks without becoming additional applications.

## Language

**Worker**:
An independently deployable agent execution process that can host multiple agents and tasks.
_Avoid_: A single fixed Worker product for every use case.

**Gateway**:
The coordination point through which Clients discover and address authorized Worker Instances and teams. Workers can also operate standalone.

**Client**:
A user-facing application that connects to Gateway through an SDK, such as a CLI, web application, or desktop application. Each is an independent application.
_Avoid_: A shared library, a mandatory unified application.

**SDK**:
A reusable client library through which Client applications connect to Gateway. Language-specific SDKs provide this role without becoming a Client application or a Worker execution engine.
_Avoid_: Worker Package SDK when referring to the client library.

**Worker runtime**:
The shared execution harness reused by different Worker Packages. It supplies common execution behavior rather than defining one fixed use case.

**Worker development launcher**:
The native ADK-Rust Launcher used to exercise a Worker through its development CLI or web server without Gateway. These development tools are distinct from Goldenbee Client applications.

**Worker Package**:
A developer-authored release containing executable capabilities for a use case. One package can create multiple independently configured Worker Instances.
_Avoid_: A prompt collection, a Runtime Profile.

**Private registry**:
An authenticated distribution service for immutable Worker Package releases. It is optional and never participates in task invocation or Worker execution.
_Avoid_: Gateway, Worker runtime, Client SDK.

**Gateway local host**:
The Gateway's own package cache and Worker Instance lifecycle: verified package installation, provisioning, inspection, start, stop, restart, upgrade, and rollback. It owns host filesystem policy; callers never receive those paths or a separate host credential.
_Avoid_: A separate composer process, a universal privileged install endpoint, a task proxy for a remote host.

**Worker Instance**:
An installation of a Worker Package with its own identity, state, permissions, resource limits, and Runtime Profiles.
_Avoid_: A package release, an agent role.

**Runtime Profile**:
User-owned, revisioned configuration selecting a Worker's agents, instructions, models, skills, workflows, connections, and restrictions. It cannot create capabilities missing from the installed package.
_Avoid_: A Worker Package, a new executable implementation.

**Logical provider**:
An operator-named model reference declared under `[providers]` in Worker instance configuration. A Runtime Profile, agent file, or package registration selects the name; the Worker resolves it to one native adapter and concrete provider and records that decision per invocation.
_Avoid_: A raw API key, a package-specific provider implementation.

**Fallback**:
An explicitly allowed alternative logical provider that may be selected only when the primary provider fails transiently, and only if locality, residency, credential authority, cost, and data handling do not widen.
_Avoid_: An automatic provider switch, a silent retry.

**Context compaction**:
Native ADK Runner reduction of the active model context before a request exceeds the selected provider's declared window. Canonical session events remain the source history; the harness records a task event and trace instead of rewriting history or installing its own summarizer.
_Avoid_: A harness conversation database, a Package summarization loop.

**Master**:
A Worker Instance assigned responsibility for coordinating a particular team execution and evaluating delegated results.
_Avoid_: A separate Master executable or permanent Worker type.

**Subworker**:
A Worker Instance receiving delegated work under the parent's authority and limits.
_Avoid_: A different kind of Worker binary.

**Team**:
A versioned, authorized roster of Worker Instances with a designated Master and bounded delegation rules.
