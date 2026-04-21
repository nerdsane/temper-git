// Webhook Cedar policies — outbound HTTP notifications on repo events.
//
// Webhooks reveal events to an external URL; managing them is a
// repo-admin-shaped capability. We gate on admin:repos for the
// target repo (or owner of the repo). RecordDelivery is fired by the
// webhook-delivery integration and is system-only.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Webhook
) when {
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action in [
    Action::"Create",
    Action::"Pause",
    Action::"Resume",
    Action::"UpdateConfig",
    Action::"Delete"
  ],
  resource is Webhook
) when {
  (context.repositoryOwnerAccountId == principal.accountId ||
   principal.scopes.contains("admin:repos")) &&
  (principal.scopes.contains("admin:repos") ||
   principal.scopes.contains("repo:write"))
};

// --- RecordDelivery: system-only (webhook-delivery integration).

permit(
  principal,
  action == Action::"RecordDelivery",
  resource is Webhook
) when {
  principal.agent_type == "system"
};
