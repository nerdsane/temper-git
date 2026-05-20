// Commons-mode App guardrails.
//
// The root app.cedar policy remains permissive for operator mode. These
// commons overlays prevent callers from creating/updating registry rows
// directly and force the public workflow through spec-declared App composites.

forbid(
  principal,
  action == Action::"Create",
  resource is App
) when {
  !(principal has action_context &&
    (principal.action_context == "composite:App.RegisterNewApp" ||
     principal.action_context == "composite:App.Fork"))
};

// Composite-created App rows must still belong to the caller's owner account.
// Forking someone else's public App is allowed, but creating the child App under
// another owner is not.
forbid(
  principal,
  action == Action::"Create",
  resource is App
) when {
  principal has action_context &&
  (principal.action_context == "composite:App.RegisterNewApp" ||
   principal.action_context == "composite:App.Fork") &&
  !(resource.OwnerId == principal.accountId ||
    (principal has scopes &&
     principal.scopes.contains("admin:repos")))
};

forbid(
  principal,
  action == Action::"Update",
  resource is App
) when {
  !(principal has action_context &&
    principal.action_context == "composite:App.PublishNewVersion")
};

// Publishing, deprecating, deleting, and the publish composite's App.Update
// sub-write are owner-scoped in commons mode. The base app.cedar stays
// permissive for operator mode; these forbids are the commons overlay.
forbid(
  principal,
  action in [
    Action::"PublishNewVersion",
    Action::"Deprecate",
    Action::"Delete",
    Action::"Update"
  ],
  resource is App
) when {
  !(resource.OwnerId == principal.accountId ||
    (principal has scopes &&
     principal.scopes.contains("admin:repos")))
};
