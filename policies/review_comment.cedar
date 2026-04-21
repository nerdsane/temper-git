// ReviewComment Cedar policies — line-level comments on a Review.
//
// Create: any pr:write principal who is the comment author. Resolve:
// the author OR the PR author OR admin:repos.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is ReviewComment
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action == Action::"Create",
  resource is ReviewComment
) when {
  principal.scopes.contains("pr:write") &&
  resource.AuthorPrincipal == principal.id
};

permit(
  principal,
  action == Action::"Resolve",
  resource is ReviewComment
) when {
  resource.AuthorPrincipal == principal.id ||
  context.pullRequestOpenedBy == principal.id ||
  principal.scopes.contains("admin:repos")
};
