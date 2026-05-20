// PullRequest Cedar policies — draft, open, review, merge, close.

// --- Read: same visibility rules as the owning repo.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is PullRequest
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

// --- Create/Open: pr:write + principal is OpenedBy, or admin:repos.

permit(
  principal,
  action in [Action::"Create", Action::"Open"],
  resource is PullRequest
) when {
  principal.scopes.contains("admin:repos") ||
  (principal.scopes.contains("pr:write") &&
   resource.OpenedBy == principal.id)
};

// --- Receive-pack updates PR heads only from the Repository.IngestPack
//     composite. The git wire adapter never dispatches this action.

permit(
  principal,
  action == Action::"UpdateHead",
  resource is PullRequest
) when {
  principal has action_context &&
  principal.action_context == "composite:Repository.IngestPack"
};

// --- Author actions: only OpenedBy may request review on their own PR.

permit(
  principal,
  action == Action::"RequestReview",
  resource is PullRequest
) when {
  resource.OpenedBy == principal.id &&
  principal.scopes.contains("pr:write")
};

// --- Close: OpenedBy or admin:repos.

permit(
  principal,
  action == Action::"Close",
  resource is PullRequest
) when {
  (resource.OpenedBy == principal.id ||
   principal.scopes.contains("admin:repos")) &&
  principal.scopes.contains("pr:write")
};

// --- Review transitions: any pr:write principal who is NOT the author.

permit(
  principal,
  action in [
    Action::"Approve",
    Action::"RequestChanges",
    Action::"DismissReview"
  ],
  resource is PullRequest
) when {
  principal.scopes.contains("pr:write") &&
  resource.OpenedBy != principal.id
};

forbid(
  principal,
  action in [
    Action::"Approve",
    Action::"RequestChanges"
  ],
  resource is PullRequest
) when {
  resource.OpenedBy == principal.id
};

// --- Merge: pr:merge scope. The state machine gates state (Approved).

permit(
  principal,
  action == Action::"Merge",
  resource is PullRequest
) when {
  principal.scopes.contains("pr:merge")
};
