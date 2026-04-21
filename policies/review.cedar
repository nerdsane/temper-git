// Review Cedar policies.
//
// A Review is a reviewer's verdict on a PR. Reviewer identity is
// captured as ReviewerPrincipal on the row; Cedar ensures only that
// principal can Dismiss their own review (authors cannot silence
// reviews against them).

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Review
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

// --- Submit: any pr:write principal who is NOT the PR author.

permit(
  principal,
  action == Action::"Submit",
  resource is Review
) when {
  principal.scopes.contains("pr:write") &&
  resource.ReviewerPrincipal == principal.id &&
  context.pullRequestOpenedBy != principal.id
};

forbid(
  principal,
  action == Action::"Submit",
  resource is Review
) when {
  context.pullRequestOpenedBy == principal.id
};

// --- Dismiss: the reviewer themselves, or admin:repos.

permit(
  principal,
  action == Action::"Dismiss",
  resource is Review
) when {
  resource.ReviewerPrincipal == principal.id ||
  principal.scopes.contains("admin:repos")
};
