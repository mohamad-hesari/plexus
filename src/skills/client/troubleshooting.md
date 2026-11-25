# Troubleshooting `plexus client`

**Exit 2, "no running plexus found".** No `plexus run -W` serves this project. The client
looks for `node_modules/plexus.sock` (Windows: `node_modules/plexus.name`) from the current
directory upwards. Run from inside the repo, or pass `--root <repo>`. Plexus only serves
clients in watch mode (`-W`). Fall back to running scripts yourself, or ask the user to start
plexus.

**Exit 2, "the socket file is stale".** The plexus that created the socket has exited or
crashed. The next `plexus run -W` replaces it. Don't delete it yourself.

**Exit 2, permission denied.** The socket is owner-only by default. Inside an unprivileged
container the uid is shifted, so the container user is not the owner. The user can map the uid
(Incus: `shift=true` on the disk device) or start plexus with a wider `--socket-mode`, e.g. 666.

**Using TCP instead of the socket.** If plexus was started with `--control-tcp ADDR`, connect
with `{{plexus}} client --tcp HOST:PORT ...`. The token comes from `node_modules/plexus.token`,
or `--token-file <path>`. "invalid or missing token" means the token doesn't match this run.

**"has no loaded command".** Plexus only runs the scripts it was started with (`-C`), for the
packages it was started with (`-f`). `{{plexus}} client list` shows them.

**`restart` hangs.** The script is long-lived (a dev server or watcher). Interrupt it and use
`--no-wait`.

**"did not stop within 5s" / "still stopping".** The process ignored the stop signal for 5
seconds. Wait a moment and retry. Check `status` for the pid.

**`blocked: dependency X failed`.** The same script failed in a package this one depends on.
Run `{{plexus}} client logs X:<script>`, fix it, then `restart X:<script>` before this one.

**A port is missing from `ports`.** Ports are rescanned every 2 seconds, so check again
shortly after a start. Only processes started by that script are scanned. A process that
detaches from its parent is not tracked.
