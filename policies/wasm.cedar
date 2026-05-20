// WASM host-function policies.
//
// Repository.IngestPack's parser integration may read local TData so it can
// resolve existing delta bases and find open PRs whose SourceRef matches an
// incoming ref update. It still returns data only; the kernel applies writes
// through the Repository.IngestPack Composite contract.

permit(
  principal is Agent,
  action == Action::"http_call",
  resource is HttpEndpoint
) when {
  context.module == "scm_ingest_pack" &&
  context.method == "GET" &&
  ["127.0.0.1", "localhost"].contains(context.domain)
};

// App.Fork's registry integration may read the parent App row. It returns
// declared sub-write data only; the kernel applies App.Fork's Composite
// contract.

permit(
  principal is Agent,
  action == Action::"http_call",
  resource is HttpEndpoint
) when {
  context.module == "app_registry" &&
  context.method == "GET" &&
  ["127.0.0.1", "localhost"].contains(context.domain)
};
