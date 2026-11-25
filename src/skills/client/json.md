# `--json` output of `plexus client`

With `--json`, every line on stdout is one JSON object with a `type` field. Exit codes stay
the same: 0 success, 1 failed or rejected, 2 no plexus reachable.

Statuses are `Init`, `Starting`, `Running`, `Successed`, `Failed`, `Stopping`, `Stopped`.

## `list`

```json
{"type":"packages","packages":[{"name":"@acme/web","path":"packages/web","commands":["dev","tsc"]}]}
```

## `status`, `ports`

```json
{"type":"commands","commands":[{"package":"@acme/web","command":"dev","script":"dev","status":"Running","pid":4242,"ports":[{"addr":"127.0.0.1:3000","pid":4250,"process":"node","alive":true}],"runs":1}]}
```

`runs` counts how many times the script has started. `blocked_by` appears on a command in
`Init` whose dependency failed. `alive` is false when the port is listed but refused a test
connection. `ports` returns the same shape as `status`; filter for non-empty `ports` yourself.

## `logs`, `start`, `restart`, `stop`

Output lines first, then one closing object:

```json
{"type":"log","target":"@acme/web:tsc","line":"[OUT]: src/a.ts(3,1): error TS2304"}
{"type":"done","results":[{"target":"@acme/web:tsc","status":"Failed"}]}
```

`line` starts with `[OUT]: ` for stdout or `[ERR]: ` for stderr. `logs` ends with
`{"type":"ok"}` instead of `done`. A result can carry a `note`: `"triggered"` with
`--no-wait`, `"already running"` when `start --no-wait` found the command running and started
nothing, `"not running"` from `stop`, or `"blocked: dependency @acme/ui failed"`.

## `forward`, `forwards`, `unforward`

```json
{"type":"forward","id":3,"listen":"127.0.0.1:3000","package":"web","port":3000,"pid":5151}
{"type":"forwards","forwards":[{"id":3,"package":"@acme/web","port":3000,"listen":"127.0.0.1:3000","since":"2026-01-01T10:00:00Z"}]}
{"type":"stopped","count":1}
```

`restart-all` replies `{"type":"restarting","count":7}` and returns without waiting.

`pid` is present with `--detach` only.

## Errors

```json
{"type":"error","message":"package '@acme/web' has no loaded command 'lint'; loaded: dev, tsc"}
```
