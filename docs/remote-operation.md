# Remote collection and viewing

## First-version scope: existing commands plus SSH tunneling

Run collection directly on the remote system, using its normal shell or scheduler
workflow. Run `rprof view` where the capture files are accessible, then forward its
loopback HTTP port to the local browser with SSH. The indexed viewer serves both
the HTML and bounded window/file-page queries through this same tunnel.

```text
remote system: workload → rprof record → saved capture → rprof view
                                                           ↕ loopback HTTP
local browser ← localhost port ← SSH tunnel ────────────────┘
```

The user manages workload launch, allocation, and the SSH connection. Collection
and viewing are separate invocations. Existing shell/scheduler facilities manage
long-running jobs; the foreground `record` command does not provide detached remote
session management.

## Workflow

On the remote system, within the appropriate compute-node allocation:

```sh
rprof record -o capture.rprof.jsonl -- ./application
rprof view capture.rprof.jsonl --port 8080
```

From the local machine, forward the port on the host running the viewer:

```sh
ssh -N -L 127.0.0.1:8080:127.0.0.1:8080 remote-host
```

Open `http://127.0.0.1:8080` locally. Replace `remote-host` with the appropriate SSH
alias. If a jump host is required, for example to reach an allocated H100 node:

```sh
ssh -N -J tst9 \
  -L 127.0.0.1:8080:127.0.0.1:8080 allocated-h100-node
```

Replace `allocated-h100-node` with the actual node name. Use `tst4` and the selected
`hbv4` node for MPI testing. The viewer can also run on another suitable host with
access to shared capture storage; forward to that viewer host.

HTTP inside the SSH tunnel is sufficient: SSH encrypts the network connection.
`rprof view` stays bound to loopback. It builds/reuses a disposable index and serves
windowed data from a saved snapshot;
browser refresh does not re-read the capture. Re-run the viewer for a newer
snapshot. Existing SSH configuration handles aliases, keys, and jump hosts.

See [indexed viewing](indexed-viewer.md) for cache placement and query limits.
Standalone HTML still works without a server; its retained detail can differ from
an indexed window. There is no remote job-control API, worker service or tunnel automation.

## Later feature: open remote files from a local GUI

A useful follow-up is a **read-only remote-file source** for the local GUI:

- Select an SSH host and an existing remote capture file.
- Fetch/cache the capture using SSH file transfer, then reuse local analysis and
  rendering. Include transfer progress and clear cache/source identity.
- Start with completed files; consider resumable transfers or bounded remote reads
  if real capture sizes justify them.

This feature opens existing data; it does not launch or manage remote collection.
Choose its transport and GUI details when implementing it. A local backend would
handle SSH access because browser JavaScript cannot directly use SSH credentials.

The earlier proposal for remote job orchestration and persistent worker/session
services is superseded by this simpler scope.
