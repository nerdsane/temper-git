// Repository Cedar policies — gates create / write / archive on the
// repo object itself. Object-CAS writes are gated in objects.cedar,
// ref updates in ref.cedar, PR flow in pull_request.cedar.
//
// Scope model mirrors GitHub fine-grained PATs:
//   repo:read, repo:write, pr:write, pr:merge,
//   admin:repos, admin:tokens, force.
// Scopes are carried on the Principal's `scopes` attribute (populated
// by the auth integration from the matching GitToken row).
//
// Cedar principal type: Agent. Humans map to Agent with
// agent_type == "human"; the kernel enforces that the bearer token
// resolved to that principal.

// --- Read: any authenticated principal with repo:read on a public
//     repo, or explicit collaborator on a private repo.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Repository
) when {
  resource.Visibility == "public" ||
  resource.OwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

// --- Create: any principal with repo:write creating under their own
//     account, or admin:repos creating under any account.

permit(
  principal,
  action == Action::"Create",
  resource is Repository
) when {
  principal.scopes.contains("admin:repos") ||
  (principal.scopes.contains("repo:write") &&
   resource.OwnerAccountId == principal.accountId)
};

// --- MarkProvisioned: only the repository_provision integration
//     (system principal) can transition Provisioning -> Active.

permit(
  principal,
  action == Action::"MarkProvisioned",
  resource is Repository
) when {
  principal.agent_type == "system"
};

// --- File/tree mutations — repo:write (or admin:repos) + same owner.

permit(
  principal,
  action in [
    Action::"WriteFile",
    Action::"DeleteFile",
    Action::"BatchWriteFiles",
    Action::"CreateBranch",
    Action::"SetDefaultBranch"
  ],
  resource is Repository
) when {
  (principal.scopes.contains("repo:write") ||
   principal.scopes.contains("admin:repos")) &&
  (resource.OwnerAccountId == principal.accountId ||
   principal.scopes.contains("admin:repos"))
};

// --- MergePullRequest: pr:merge scope + collaborator or admin.

permit(
  principal,
  action == Action::"MergePullRequest",
  resource is Repository
) when {
  principal.scopes.contains("pr:merge") &&
  (resource.OwnerAccountId == principal.accountId ||
   principal.scopes.contains("admin:repos"))
};

// --- Archive: admin:repos only. Terminal, destructive.

permit(
  principal,
  action == Action::"Archive",
  resource is Repository
) when {
  principal.scopes.contains("admin:repos")
};

// --- Defense-in-depth: archived repos reject all writes even if a
//     scope grant would otherwise permit it.

forbid(
  principal,
  action in [
    Action::"WriteFile",
    Action::"DeleteFile",
    Action::"BatchWriteFiles",
    Action::"CreateBranch",
    Action::"SetDefaultBranch",
    Action::"MergePullRequest"
  ],
  resource is Repository
) when {
  resource.Status == "Archived"
};
