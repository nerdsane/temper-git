// Commons-mode Ref guardrails.
//
// App composites create or advance backing refs as part of one spec-declared
// registry intent. The normal repo-write policy still covers ordinary git
// pushes; these permits cover registry metadata flows where the child repo/ref
// is being introduced by the composite itself.

permit(
  principal,
  action == Action::"Create",
  resource is Ref
) when {
  principal has action_context &&
  principal.action_context == "composite:App.Fork"
};

permit(
  principal,
  action == Action::"Update",
  resource is Ref
) when {
  principal has action_context &&
  principal.action_context == "composite:App.PublishNewVersion"
};
