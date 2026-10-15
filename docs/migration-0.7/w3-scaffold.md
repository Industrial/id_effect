# `fork_daemon` inherits `FiberRef` values

`Effect::fork_daemon` (in `id_effect::next`) now starts the daemon with a copy of the forking
fiber's `FiberRef` values at fork time, like Effect.ts `forkDaemon`. Before, a daemon started
from the initial values. Only the lifetime is detached: the daemon still has no parent, and a
`set` on either side stays invisible to the other.
