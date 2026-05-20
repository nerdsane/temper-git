// App Cedar policies.
//
// Operator mode is permissive while the registry shape lands. Commons mode
// later narrows Create/Register/Fork/Publish to owner-scoped principals and
// composite action_context checks.

permit(principal, action, resource is App);
permit(principal, action, resource is AppInstallation);
