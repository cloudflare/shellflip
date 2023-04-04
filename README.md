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

## License

BSD licensed. See the [LICENSE](LICENSE) file for details.

🦀ノ( º _ ºノ) - respect crables!
