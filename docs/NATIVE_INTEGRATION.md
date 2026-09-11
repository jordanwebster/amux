# Native application integration

Extract the bridge from nativeapp's real code into three packages. `app-runtime`
coordinates rich-client sessions, fleet cache and typed presentation updates;
it depends on `client`, `ui-state` and `ui-runtime`, never `node`.
`embedded-client` owns `node::Installation`, opens an embedded profile with no
local-agent host factory, returns its explicit channel and shuts down only that
owned installation. `client-ffi` owns C callback and serialization boundaries.

Keep cache and projection as modules until another consumer proves a package
boundary. Rust APIs remain typed; serialization happens at the C bridge. Every
operation carries an account/profile identity, and every view and subscription
has an independent owner handle. Dropping an attached view disconnects it and
does not stop an external daemon. Platform shells own active selection,
navigation, windows and background/activity policy.

Each build configuration selects exactly one bridge. Simulator iteration builds
only its required architecture. Shipping graphs exclude agent runtime, test
agents, replay support and provider tools. Each worktree owns its simulator,
Cargo outputs and an explicit native artifact manifest. Evaluate Bazel only
against the integrated Rust–Swift application.
