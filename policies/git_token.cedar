// GitToken Cedar policies.
//
// Token creation and revocation are the sharpest surface in the app —
// a rogue Create grants a long-lived bearer credential. Rules:
//
//   * Create / Revoke by a principal for their OWN tokens requires
//     admin:tokens (a principal minting their own tokens is fine if
//     they already have the scope; it's how rotation works).
//   * Create / Revoke by a principal on someone ELSE's tokens
//     requires admin:tokens AND the actor is a system/admin agent
//     (support path, audited).
//   * MarkUsed is fired by the auth integration on every successful
//     auth — system-only.
//
// Read: the token's own principal, or admin:tokens.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is GitToken
) when {
  resource.PrincipalId == principal.id ||
  principal.scopes.contains("admin:tokens")
};

// --- Create: own-principal tokens + admin:tokens scope.

permit(
  principal,
  action == Action::"Create",
  resource is GitToken
) when {
  resource.PrincipalId == principal.id &&
  principal.scopes.contains("admin:tokens")
};

// --- Create on behalf of another principal: admin-only, audited.

permit(
  principal,
  action == Action::"Create",
  resource is GitToken
) when {
  resource.PrincipalId != principal.id &&
  principal.scopes.contains("admin:tokens") &&
  principal.agent_type == "admin"
};

// --- Revoke: own principal, or admin:tokens.

permit(
  principal,
  action == Action::"Revoke",
  resource is GitToken
) when {
  resource.PrincipalId == principal.id ||
  principal.scopes.contains("admin:tokens")
};

// --- MarkUsed: system principal only (auth integration).

permit(
  principal,
  action == Action::"MarkUsed",
  resource is GitToken
) when {
  principal.agent_type == "system"
};

// --- Defense-in-depth: revoked tokens are inert. The state machine
//     also enforces this; Cedar catches any bypass attempt.

forbid(
  principal,
  action,
  resource is GitToken
) when {
  resource.Status == "Revoked" &&
  action != Action::"read" &&
  action != Action::"list"
};
