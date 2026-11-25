# Reaching a dev server's port through plexus

When you run in a container or sandbox that shares the project files but not the host's
network, `localhost:3000` from the host is unreachable. `forward` opens a local port and
relays every connection through plexus to the dev server.

```bash
{{plexus}} client ports web                                 # find the port, e.g. 127.0.0.1:3000
{{plexus}} client forward web 3000 --detach                 # listen on 127.0.0.1:3000 here
{{plexus}} client forward web 3000 --listen 127.0.0.1:4000 --detach   # or pick another local port
curl http://127.0.0.1:3000/
```

- `--detach` runs the forward in the background and returns once it is listening. It prints
  `forward <id>: <listen> -> <package>:<port>`. Without `--detach` it blocks until stopped.
- Use `--listen` when the port is already taken locally. `0.0.0.0:<port>` exposes it to other
  machines too, so only do that if you mean to.
- Only ports that `ports` lists for that package are relayed. If the dev server is still
  starting, the forward can be created now and will relay once the port shows up.
- A forward relays any TCP protocol (HTTP, WebSocket, a database port). Each incoming
  connection gets its own relay, so parallel requests work.

## Stop forwards

Always stop the forwards you started when you are done.

```bash
{{plexus}} client forwards                 # id, listen address, target, start time
{{plexus}} client unforward 3              # by id
{{plexus}} client unforward web:3000       # by package and port
{{plexus}} client unforward web            # all forwards of a package
{{plexus}} client unforward --all
```

`unforward` exits 1 if nothing matched. Forwards also end on their own when plexus exits.
