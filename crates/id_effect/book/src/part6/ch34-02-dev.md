# The dev loop

## `id-effect dev`

```text
id-effect dev [--path DIR] [--package NAME] [--bin NAME] [--watch PATH]... [--services]
              [--poll-ms MS] [--debounce-ms MS] [--grace-secs SECS] [-- APP ARGS...]
```

`id-effect dev` builds the project in `DIR` (default: the current directory), runs its binary with `ID_EFFECT_PROFILE=dev` (so `id_effect_config` profiles load `config/dev.toml`; see [Config profiles](../part3/ch16-03-config-profiles.md)), and rebuilds and restarts it whenever a watched file changes:

1. Every `--poll-ms` (default 500) it compares the modification time and length of every file under `src/`, `Cargo.toml`, `config/` and each `--watch PATH` with the previous scan. It skips `target`, hidden entries (`.git`, `.idea`, …) and editor backups (`*~`, `*.swp`, `*.swx`).
2. A change waits until a scan finds the files quiet for `--debounce-ms` (default 200), so a save that touches several files, or a formatter run, rebuilds once.
3. It runs `cargo build` (with `--package` / `--bin` when given). The compiler's output streams to the terminal.
4. Only when the build succeeds is the running app stopped: SIGTERM, then up to `--grace-secs` (default 10) to drain in-flight work and exit, after which it is killed. Then the new binary starts, with the arguments after `--`.

A failed build keeps the running app: you see the compiler errors and keep using the last good version until the next change fixes it.

```text
$ id-effect dev -- --port 8080
id-effect dev: watching src, Cargo.toml, config every 500ms
id-effect dev: cargo build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.21s
id-effect dev: started /home/me/orders-api/target/debug/orders-api (pid 41022, ID_EFFECT_PROFILE=dev)
… the app's output …
id-effect dev: changed src/routes.rs; rebuilding
id-effect dev: cargo build
error[E0425]: cannot find value `ordr` in this scope
id-effect dev: build failed (`cargo build` exit status: 101); keeping the running app (pid 41022); waiting for a change
id-effect dev: changed src/routes.rs; rebuilding
id-effect dev: cargo build
id-effect dev: stopped pid 41022 (signal: 15 (SIGTERM))
id-effect dev: started /home/me/orders-api/target/debug/orders-api (pid 41107, ID_EFFECT_PROFILE=dev)
```

`--services` first runs `docker compose up -d` in `DIR` when it has a compose file (`compose.yaml`, which every generated project has, `compose.yml`, `docker-compose.yaml` or `docker-compose.yml`), so the database and broker are up before the app starts. Without a compose file it says so and carries on; a failing `docker compose` stops `id-effect dev` with exit status 1.

When the build makes more than one binary (a workspace, or a package with several `[[bin]]` targets), `id-effect dev` asks for `--package` or `--bin` instead of guessing.

What `id-effect dev` does not do:

- It polls; it does not use file system notifications (the `notify` crate's license is not on the workspace's allow list). A scan of a project's `src/` and `config/` costs little, but a large `--watch` directory is rescanned every `--poll-ms`.
- It restarts the process; nothing is hot-swapped in a running app, and the new process binds the port again after the old one released it. An app that drains slowly delays the restart by up to `--grace-secs`.
- On Windows the old app is killed at once (there is no SIGTERM).
- Ctrl-C stops `id-effect dev` and the app together (they share the terminal's process group); the app sees the interrupt like any Ctrl-C.
- The app's stdin, stdout and stderr are the terminal's.
