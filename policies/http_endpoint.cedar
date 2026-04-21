// HttpEndpoint Cedar policies — kernel-delta entity that registers a
// WASM integration as a path-prefix HTTP route (see RFC-0001 K-1).
//
// Routing extensions reshape the attack surface of the whole tenant,
// so this is platform-admin-only. No repo-scoped grant can reach it.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is HttpEndpoint
) when {
  principal.scopes.contains("admin:platform") ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action in [
    Action::"Create",
    Action::"Pause",
    Action::"Resume",
    Action::"Delete"
  ],
  resource is HttpEndpoint
) when {
  principal.scopes.contains("admin:platform") &&
  principal.agent_type == "admin"
};
