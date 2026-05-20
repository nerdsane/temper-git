// Closure Cedar policies.
//
// Closures are content-addressed lockfile rows. They are broadly readable and
// operator-writable in Phase 3; commons mode adds per-owner rate limits.

permit(principal, action, resource is Closure);
