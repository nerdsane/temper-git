// Git object Cedar policies — Blob, Tree, TreeEntry, Commit, Tag.
//
// Git objects are append-only and content-addressed by SHA-1: their
// identity IS their bytes. Cedar here is only a read gate, piggy-backed
// on the owning repository's visibility. Writes are not addressed
// here — object rows are produced exclusively by the scm_write_file /
// scm_merge_pr / git_receive_pack integrations, which run as system
// principals.
//
// Reads: any principal that can read Repository with the same
// RepositoryId can read the object.

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Blob
) when {
  resource.RepositoryId == context.repositoryId &&
  context.repositoryVisibility == "public"
} unless {
  principal.agent_type == "anonymous" &&
  context.repositoryVisibility != "public"
};

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Blob
) when {
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Tree
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is TreeEntry
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Commit
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

permit(
  principal,
  action in [Action::"read", Action::"list"],
  resource is Tag
) when {
  context.repositoryVisibility == "public" ||
  context.repositoryOwnerAccountId == principal.accountId ||
  principal.scopes.contains("admin:repos")
};

// --- Writes: only system principals (integrations) may create object
//     rows. Human / agent writes flow through Repository actions
//     (WriteFile, BatchWriteFiles) or through git_receive_pack, which
//     itself runs as system after auth.

permit(
  principal,
  action in [Action::"Create"],
  resource is Blob
) when {
  principal.agent_type == "system"
};

permit(
  principal,
  action in [Action::"Create"],
  resource is Tree
) when {
  principal.agent_type == "system"
};

permit(
  principal,
  action in [Action::"Create"],
  resource is TreeEntry
) when {
  principal.agent_type == "system"
};

permit(
  principal,
  action in [Action::"Create"],
  resource is Commit
) when {
  principal.agent_type == "system"
};

permit(
  principal,
  action in [Action::"Create"],
  resource is Tag
) when {
  principal.agent_type == "system"
};
