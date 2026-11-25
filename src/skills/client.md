# Driving a running plexus session

A developer started `plexus run -W` in this monorepo. It runs pnpm scripts in dependency order,
reruns them when files change, and keeps dev servers alive. Ask it to run checks and to show
output, instead of running `tsc`, `lint`, `build` or `dev` yourself. Running them yourself
duplicates its work, and starting a second dev server fights over the same port.

## Start here

```bash
{{plexus}} client list
```

This shows the packages plexus loaded and the scripts it can run for each. Only those are
reachable. If a script you need is not listed, plexus was not started with it: run that one
yourself, or ask the user to restart plexus with it.

Exit code 2 from any `client` command means no plexus is running here. Fall back to running
the scripts yourself.

## Targets

A target is `package:script`, for example `@acme/web:tsc`. A unique short name works too
(`web:tsc` for `@acme/web`). A bare package (`web`) means every loaded script of that package.

## Run a check and get its result

```bash
{{plexus}} client restart web:tsc
```

This blocks, streams the script's output (stdout to stdout, stderr to stderr), and exits 0 if
it succeeded or 1 if it failed, just like running the script directly. A final line on stderr
gives the status, e.g. `@acme/web:tsc: Failed`.

`{{plexus}} client start web:tsc` does the same, except that it follows a run already in
progress instead of killing it.

Plexus also reruns scripts on its own when files change. After editing, `restart` is still the
reliable way to get a result for your edit.

A script waits until the same script has succeeded in every package it depends on. If one of
them failed, `restart` exits 1 with `blocked: dependency <pkg> failed`. Fix and restart that
package's script first.

## Long-lived scripts (dev servers)

`dev`, `start`, `serve` and other watch-style scripts never finish, so never wait on them:

```bash
{{plexus}} client restart web:dev --no-wait
{{plexus}} client start web:dev --no-wait        # "already running" if it was; nothing restarts
{{plexus}} client stop web:dev
{{plexus}} client status web:dev                # Running? pid? ports?
```

## Read output

```bash
{{plexus}} client logs web:dev -n 50            # last 50 lines (--tail 50)
{{plexus}} client logs web:dev -f               # keep streaming (--follow); stop it with Ctrl+C or a timeout
```

## Status and ports

```bash
{{plexus}} client status [target]
{{plexus}} client ports [target]
```

Statuses: `Init` (waiting to start, usually on dependencies), `Starting`, `Running`,
`Successed`, `Failed`, `Stopping`, `Stopped`. `ports` lists the TCP ports the script's
processes listen on, e.g. a dev server on `127.0.0.1:3000`.

## More

- Port not reachable from where you run (container, sandbox)? Run `{{plexus}} skills get client/forwarding`.
- Parsing output in a script? Add `--json`, then run `{{plexus}} skills get client/json`.
- Errors such as exit 2, permission denied, a stale socket? Run `{{plexus}} skills get client/troubleshooting`.

Exit codes: 0 success, 1 the command failed or the request was rejected, 2 no plexus reachable.
