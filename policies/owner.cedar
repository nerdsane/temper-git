// Owner Cedar policies.
//
// Phase 3 keeps operator mode permissive. Commons-specific ownership,
// verification, quota, and rate-limit rules are layered in Phase 5.

permit(principal, action, resource is Owner);
