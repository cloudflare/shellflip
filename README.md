# shellflip
[![crates.io](https://img.shields.io/crates/v/shellflip.svg)](https://crates.io/crates/shellflip)
[![docs.rs](https://docs.rs/shellflip/badge.svg)](https://docs.rs/shellflip)

Graceful process restarts in Rust.

This crate facilitates upgrading or reconfiguring a service without disrupting existing connections.
This is achieved by forking the process and communicating a small amount of state between the old
and new processes; once the new process has started successfully the old process may terminate.

This crate has the following goals:

* No old code keeps running after a successful upgrade (and inevitable shutdown of the old process)
* The new process has a grace period for performing initialisation
* Crashing during initialisation is OK
* Only a single upgrade is ever run in parallel
* It is possible for the user/process initiating the upgrade to know if the upgrade succeeded

Inspired by the [tableflip](https://github.com/cloudflare/tableflip) go package but not a direct
replacement.

# Using the library

A full example is given in the [restarter example service](examples/restarter.rs).

The main struct of interest is `RestartConfig` which has methods for detecting or initiating
restart. For shutting down a restarted process, the `ShutdownCoordinator` provides the means for
both signalling a shutdown event to spawned tasks, and awaiting their completion.

## Using shellflip with systemd

If you are running your process as a systemd service, you may use shellflip to fork a new instance
of the process. This allows you to upgrade the binary or apply new configuration, but you cannot
change the environment or any systemd-set configuration without stopping the service completely.

If this is insufficient for your needs, shellflip has an alternative runtime model that works with
systemd's process management model. When issuing a `systemctl restart` or using the restart
coordination socket, shellflip can pass data intended for the new process to systemd, then receive
that data in the newly started instance. This requires systemd's file descriptor store to be
enabled; an example `.service` file can be found in the `examples/` directory, or you may test it as
a one-off command using `systemd-run`:

`systemd-run -p FileDescriptorStoreMax=4096 target/debug/examples/restarter --systemd`

The limitations of systemd's process management remain; the old process must terminate before the
new process can start, so all tasks must end promptly and any child processes must terminate or
accept being ungracefully killed when the parent terminates.

If you need to prevent restarting the service if the service cannot successfully serialize its
state, use ExecReload and the restart coordination socket like the non-systemd-aware shellflip mode.
Make sure that systemd restarts your service on successful exit and/or force your service to
terminate with a non-zero error code on restart.

## License

BSD licensed. See the [LICENSE](LICENSE) file for details.

🦀ノ( º _ ºノ) - respect crables!
