// PullRequest Cedar policies — open, review, merge, close.
//
// Model:
//   - Anyone with pr:write on the target repo can Open / UpdateDraft /
//     MarkReady / RequestReview / Reopen / Close their own PRs.
//   - Review transitions (RecordApproval, RecordChangesRequested)
//     require being a distinct principal from OpenedBy (defense in
//     depth — same rule lives in pull_request.ioa.toml).
//   - Merge requires pr:merge scope AND the PR must be in Approved.

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

// --- Open: pr:write + principal is OpenedBy.

permit(
  principal,
  action == Action::"Open",
  resource is PullRequest
) when {
  principal.scopes.contains("pr:write") &&
  resource.OpenedBy == principal.id
};

// --- Author actions: only OpenedBy may update/mark-ready/reopen their
//     own PR.

permit(
  principal,
  action in [
    Action::"UpdateDraft",
    Action::"MarkReady",
    Action::"RequestReview",
    Action::"Reopen"
  ],
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

// --- Review transitions: any principal with pr:write who is NOT the
//     author. Identity of reviewer is captured on the Review row,
//     not here.

permit(
  principal,
  action in [
    Action::"RecordApproval",
    Action::"RecordChangesRequested"
  ],
  resource is PullRequest
) when {
  principal.scopes.contains("pr:write") &&
  resource.OpenedBy != principal.id
};

forbid(
  principal,
  action in [
    Action::"RecordApproval",
    Action::"RecordChangesRequested"
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
