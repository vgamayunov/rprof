# Remote collection and viewing — planned

## Recommendation

Support both deployment modes through a common capture/query/session API:

1. **Remote backend with an SSH-forwarded browser port:** the smallest first step;
   computation and capture files stay near the workload.
2. **Local backend with an SSH-connected remote worker:** local session management,
   caching, reports, and browser UI, with collection and optional queries on the
   target. This is the longer-term workstation workflow.

Use the installed OpenSSH client and existing SSH configuration for both modes,
including aliases, identity selection, agent integration, known hosts, and
`ProxyJump`. Collection writes locally on the target; network transport stays off
collector callback paths. Disconnecting a viewer should not implicitly stop a job.

## Mode A: remote backend, local browser

```text
compute node: workload → collectors → capture shards → rprof query/view service
                                                          ↕ loopback HTTP
local browser ← localhost port ← SSH tunnel ───────────────┘
```

The remote service owns capture discovery, summary/index generation, and timeline
queries. The local browser receives report/viewport data through the tunnel. This
avoids copying a large capture to the workstation and lets users view results on
systems where data should remain remote.

**HTTP over an SSH tunnel is sufficient for the initial transport.** SSH encrypts
and authenticates the network leg; the HTTP endpoints are bound to loopback. If
direct network access is later needed, add HTTPS and application authentication as
a separate deployment option. A tunnel can forward HTTPS too, but TLS is not a
prerequisite for this SSH-based mode.

### What already works after phase 2

With `rprof` installed on an accessible target and a saved capture present there,
the current snapshot viewer can be forwarded manually. For example, from the local
machine, replace `allocated-h100-node` and the capture path with actual values:

```sh
ssh -t -J tst9 \
  -L 127.0.0.1:8080:127.0.0.1:8080 allocated-h100-node \
  'rprof view /path/to/capture.rprof.jsonl --port 8080'
```

Open `http://127.0.0.1:8080` in the local browser. `-t` provides a terminal so Ctrl-C
can stop the remote viewer cleanly. If the target is directly reachable, omit
`-J`; use the actual SSH alias for that machine. This command demonstrates port
forwarding, not automatic node discovery or job submission. The current viewer
serves an immutable snapshot; live refresh and remote session management are planned.

## Mode B: local backend, remote collection/data

```text
local browser ← loopback HTTP → local rprof backend / bounded cache
                                      ↕ versioned RPC over SSH stdio
                               remote rprof worker
                                      ↕
                            target capture store / query engine
                                      ↑
                               workload collectors
```

The local Rust backend starts or attaches to a remote worker via SSH. The browser
never needs SSH credentials or a direct connection to the cluster. It talks to its
local backend using the same HTTP API as mode A.

Two data strategies can share this mode:

- **Fetch and analyze locally:** transfer finalized captures or committed chunks
  into a bounded local cache, then use the existing Rust report engine. Good for
  offline work and captures that comfortably fit on the workstation.
- **Query remotely:** keep raw events and indices on the target; request bounded
  summaries, selected tracks, and time-window data. Proxy the results through the
  local backend. Good for large captures or limited network bandwidth.

Begin with completed-capture transfer and status queries. Add remote viewport
queries and incremental updates when the indexing/query service is available.
Use one persistent SSH worker channel rather than launching SSH per mouse movement.

## Shared interfaces and responsibilities

### Capture source abstraction

Introduce a Rust `CaptureSource` boundary with equivalent local-file and remote
implementations. It should expose:

- Session/shard manifests, capture state, collector capabilities, and loss counters.
- Clock-domain/track metadata and stable source identity.
- Snapshot watermarks and committed chunk discovery.
- Bounded time-window/track queries and full-artifact export/fetch.
- Progress notifications and resumable cursors.

The capture format remains transport-independent. Remote protocol and browser
report versions are negotiated separately from the on-disk schema. The existing
`rprof-report` analysis and TypeScript renderer are reused; server-backed loading
extends the current embedded-model data source instead of creating another UI.

### Control and job ownership

Add a session manager owning durable job state and explicit start/status/stop
operations. A session records the command argv, working directory, selected
environment, collector configuration, target, scheduler job/allocation identifiers,
and capture locations. A browser or SSH disconnect does not cancel the workload.
Stopping collection or cancelling a scheduler job is an explicit operation.

For stdio transport, send bounded, versioned requests containing structured argv
and options to a fixed worker entry point, e.g. a planned `rprof worker --stdio`.
Keep protocol stdout separate from workload output and diagnostic stderr. Do not
construct workload shell commands by concatenating user arguments into an SSH
command string. Make start requests idempotent so retries cannot launch duplicate
jobs after an ambiguous network failure.

An on-demand worker handles connections; job supervision/persistence must survive
that worker's disconnect. The implementation may use a per-user session supervisor
or the scheduler as job owner. Define cleanup and restart reconciliation before
offering detached collection; the current foreground `record` command alone does
not provide this remote job lifecycle.

### Data transport and live viewing

- Transfer only newline-committed frames/chunks. A partial tail is not a completed
  transfer unit. Track session, shard, sequence, byte range, and content identity.
- Resume from acknowledged boundaries; reject changed/truncated/replaced source
  files instead of appending incompatible data to a local cached copy.
- Keep independent notions of collector loss, network backlog, and client-side
  display downsampling. A slow/disconnected viewer must not block instrumentation.
- Build a manifest/index with track/time coverage and committed watermarks. Serve
  bounded viewport queries and summary/downsample levels rather than embedding an
  entire large capture in every response.
- Initially use ordinary HTTP requests plus SSE for status/watermark notifications.
  Add WebSockets only if bidirectional interaction warrants them. Both HTTP and
  stdio RPC adapters should call the same query/session implementation.
- Retain exact integer values, shard namespaces, calibration metadata, and explicit
  clock domains. Network arrival time is not a synchronization measurement.
- Bound disk caches, response sizes, in-flight requests, and subscriptions; expose
  progress so partial results and delayed updates are distinguishable.

### HPC placement and installation

Distinguish SSH entry/login hosts, allocated compute nodes, workload placement,
and the capture/query service location. A service can run on a node with access to
shared capture storage without running the profiled workload on that host.

Start with explicit target aliases and existing allocations. Add Slurm integration
for allocation/job submission, node discovery, status, and cancellation later,
preserving MPI launcher/module/runtime environment and per-rank shard identity.
For GPU work use `tst9` with the `h100` partition; for MPI use `tst4` with `hbv4`
and its configured InfiniBand. Discover actual compute-node names at execution
time rather than hard-coding the existing interactive allocation.

Probe architecture, OS/glibc compatibility, `rprof`/protocol versions, and requested
collector dependencies before launch. Provide reproducible target-side install or
compatible artifacts; copying a locally built binary is not universally sufficient,
especially once MPI, OMPT, and CUPTI adapters depend on target runtimes.

## Proposed CLI experience

These commands are design sketches, not currently implemented interfaces:

```sh
rprof remote record --target cluster-node --output-dir /scratch/rprof -- ./solver
rprof remote sessions --target cluster-node
rprof remote view --target cluster-node --session UUID --mode tunnel
rprof remote view --target cluster-node --session UUID --mode local
rprof remote fetch --target cluster-node --session UUID --destination captures/
rprof remote stop --target cluster-node --session UUID
```

In tunnel mode, automate service startup, port selection, tunnel lifetime, and the
printed browser URL. In local mode, the local backend owns the UI and remote data
connection. Both modes should also attach to already-completed captures without
launching a new workload. Browser closure stops neither the remote workload nor
the local management service unless the user explicitly requests that behavior.

## Delivery sequence

1. **Document/manual tunnel workflow:** already possible for phase-2 snapshots;
   verify it on a selected remote node when deployment/testing begins.
2. **Automated remote snapshot viewing:** target capability/version probe, remote
   viewer startup, SSH port forwarding, cleanup, and local URL handling.
3. **Local-backend SSH mode:** shared source/protocol interfaces, completed-capture
   fetch, cache identity, reconnect/resume, and remote status.
4. **Remote collection lifecycle:** persistent sessions, structured start/stop,
   stdout/stderr forwarding, detached-job semantics, and idempotent retry.
5. **Indexed/live queries and HPC integration:** incremental data, browser data-source
   adapter, remote query execution, Slurm placement, and multi-node shard discovery.
6. **Optional direct HTTPS deployment:** authenticated direct access and explicit
   session ownership for environments that require a service outside SSH tunnels.

Validate both modes against the same local fixture results. Include dropped SSH
connections, reconnect after collector exit, interrupted/resumed transfers, worker
version mismatch, multiple shards, slow clients, and allocated-node replacement.
Then exercise the chosen topology on `tst9`/`h100` and `tst4`/`hbv4`. Acceptance:
capture data and loss accounting are preserved across reconnects, browser queries
are bounded, and workload/session lifetime is independent of viewer connectivity.
