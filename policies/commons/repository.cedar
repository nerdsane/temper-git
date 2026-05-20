// Commons-mode Repository guardrails.
//
// In a public commons kernel, repositories that represent Genesis apps must be
// introduced through spec-declared App composites so the matching App/Lineage
// rows are produced by the kernel from declared sub-writes.

forbid(
  principal,
  action == Action::"Create",
  resource is Repository
) when {
  !(principal has action_context &&
    (principal.action_context == "composite:App.RegisterNewApp" ||
     principal.action_context == "composite:App.Fork"))
};
