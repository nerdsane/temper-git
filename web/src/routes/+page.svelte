<script lang="ts">
  import { onMount } from 'svelte';
  import {
    AlertCircle,
    Boxes,
    Clipboard,
    Copy,
    File,
    Files,
    Folder,
    GitBranch,
    GitFork,
    GitCommitHorizontal,
    ListTree,
    Loader2,
    PackageCheck,
    RefreshCw,
    Search
  } from '@lucide/svelte';
  import {
    loadAppFiles,
    loadRegistrySnapshot,
    parseJsonList,
    parseJsonMap
  } from '$lib/api';
  import type {
    AppFilesSnapshot,
    Closure,
    Lineage,
    Owner,
    RegistryApp,
    RepositoryFile
  } from '$lib/types';

  type WorkbenchTab = 'files' | 'overview' | 'lineage' | 'install';

  let apps: RegistryApp[] = [];
  let owners: Owner[] = [];
  let lineages: Lineage[] = [];
  let closures: Closure[] = [];
  let warnings: string[] = [];

  let loading = true;
  let mounted = false;
  let loadError = '';
  let selectedAppId = '';
  let search = '';
  let statusFilter = 'all';
  let activeTab: WorkbenchTab = 'files';
  let toast = '';
  let toastTimer: number | undefined;

  let fileSnapshot: AppFilesSnapshot | null = null;
  let filesLoading = false;
  let filesError = '';
  let filesLoadKey = '';
  let currentPath = '';
  let selectedFilePath = '';

  $: ownersById = new Map(owners.map((owner) => [owner.id || owner.accountId, owner]));
  $: filteredApps = apps
    .filter((app) => {
      const query = search.trim().toLowerCase();
      const matchesQuery =
        !query ||
        app.name.toLowerCase().includes(query) ||
        app.ownerId.toLowerCase().includes(query) ||
        app.repositoryId.toLowerCase().includes(query) ||
        app.latestVersionHash.toLowerCase().includes(query);
      const matchesStatus = statusFilter === 'all' || app.status === statusFilter;
      return matchesQuery && matchesStatus;
    })
    .sort((a, b) => `${a.ownerId}/${a.name}`.localeCompare(`${b.ownerId}/${b.name}`));
  $: selectedApp =
    apps.find((app) => app.id === selectedAppId) ?? filteredApps[0] ?? apps[0] ?? null;
  $: selectedLineage = selectedApp
    ? lineages.find((lineage) => lineage.childRepositoryId === selectedApp.repositoryId) ?? null
    : null;
  $: parentApp = selectedLineage
    ? apps.find((app) => app.repositoryId === selectedLineage?.parentRepositoryId) ?? null
    : null;
  $: childLineages = selectedApp
    ? lineages.filter((lineage) => lineage.parentRepositoryId === selectedApp.repositoryId)
    : [];
  $: childApps = childLineages
    .map((lineage) => apps.find((app) => app.repositoryId === lineage.childRepositoryId))
    .filter((app): app is RegistryApp => Boolean(app));
  $: selectedClosures = selectedApp
    ? closures.filter(
        (closure) =>
          closure.root === selectedApp.id ||
          closure.root === selectedApp.latestVersionHash ||
          closure.root === selectedApp.repositoryId
      )
    : [];
  $: exportsList = selectedApp ? parseJsonList(selectedApp.exports) : [];
  $: mutationList = selectedLineage ? parseJsonList(selectedLineage.mutations) : [];
  $: fileEntries = fileSnapshot?.files ?? [];
  $: visibleEntries = entriesForPath(fileEntries, currentPath);
  $: selectedFile =
    fileEntries.find((entry) => entry.path === selectedFilePath && entry.kind !== 'directory') ??
    null;
  $: fileCount = fileEntries.filter((entry) => entry.kind !== 'directory').length;
  $: directoryCount = fileEntries.filter((entry) => entry.kind === 'directory').length;
  $: repositorySize = fileEntries.reduce((total, entry) => total + entry.size, 0);
  $: currentBreadcrumbs = breadcrumbs(currentPath);

  $: if (mounted && selectedApp) {
    const key = `${selectedApp.id}:${selectedApp.repositoryId}:${selectedApp.latestVersionHash}`;
    if (key !== filesLoadKey) {
      void loadFilesFor(selectedApp, key);
    }
  }

  $: if (mounted && !selectedApp && filesLoadKey) {
    filesLoadKey = '';
    fileSnapshot = null;
    selectedFilePath = '';
    currentPath = '';
  }

  onMount(() => {
    mounted = true;
    void refresh();
  });

  async function refresh() {
    loading = true;
    loadError = '';
    try {
      const snapshot = await loadRegistrySnapshot();
      apps = snapshot.apps;
      owners = snapshot.owners;
      lineages = snapshot.lineages;
      closures = snapshot.closures;
      warnings = snapshot.warnings.map(
        (warning) => `${warning.collection}: ${warning.message}`
      );
      if (!selectedAppId || !apps.some((app) => app.id === selectedAppId)) {
        selectedAppId = apps[0]?.id ?? '';
      }
    } catch (error) {
      loadError = error instanceof Error ? error.message : String(error);
    } finally {
      loading = false;
    }
  }

  async function loadFilesFor(app: RegistryApp, key: string) {
    filesLoadKey = key;
    filesLoading = true;
    filesError = '';
    fileSnapshot = null;
    currentPath = '';
    selectedFilePath = '';

    try {
      const snapshot = await loadAppFiles(app);
      if (filesLoadKey !== key) {
        return;
      }
      fileSnapshot = snapshot;
      currentPath = initialBrowserPath(snapshot.files);
      selectedFilePath =
        entriesForPath(snapshot.files, currentPath).find((entry) => entry.kind !== 'directory')
          ?.path ?? '';
    } catch (error) {
      if (filesLoadKey === key) {
        filesError = error instanceof Error ? error.message : String(error);
      }
    } finally {
      if (filesLoadKey === key) {
        filesLoading = false;
      }
    }
  }

  function selectApp(app: RegistryApp) {
    selectedAppId = app.id;
    activeTab = 'files';
  }

  function selectTab(tab: WorkbenchTab) {
    activeTab = tab;
  }

  function selectEntry(entry: RepositoryFile) {
    if (entry.kind === 'directory') {
      currentPath = entry.path;
      selectedFilePath = '';
      return;
    }
    selectedFilePath = entry.path;
  }

  async function copyText(value: string, label: string) {
    if (!value) {
      return;
    }
    try {
      await navigator.clipboard.writeText(value);
      showToast(`${label} copied`);
    } catch (error) {
      showToast(error instanceof Error ? error.message : 'Copy failed');
    }
  }

  function showToast(message: string) {
    toast = message;
    if (toastTimer !== undefined) {
      window.clearTimeout(toastTimer);
    }
    toastTimer = window.setTimeout(() => {
      toast = '';
    }, 2400);
  }

  function ownerLabel(ownerId: string): string {
    const owner = ownersById.get(ownerId);
    return owner?.displayName || owner?.accountId || ownerId || 'unowned';
  }

  function shortHash(value: string, length = 12): string {
    if (!value) {
      return 'pending';
    }
    return value.length > length ? `${value.slice(0, length)}...` : value;
  }

  function displayDate(value: string): string {
    if (!value) {
      return 'not recorded';
    }
    const date = new Date(value);
    if (Number.isNaN(date.valueOf())) {
      return value;
    }
    return new Intl.DateTimeFormat(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    }).format(date);
  }

  function statusClass(status: string): string {
    const normalized = status.toLowerCase();
    if (normalized.includes('verified') || normalized === 'active' || normalized === 'durable') {
      return 'green';
    }
    if (normalized.includes('pending') || normalized.includes('deprecated')) {
      return 'amber';
    }
    if (normalized.includes('suspend') || normalized.includes('delete')) {
      return 'red';
    }
    return '';
  }

  function appInitial(app: RegistryApp): string {
    return (app.name || app.id || 'G').slice(0, 1).toUpperCase();
  }

  function formatBytes(value: number): string {
    if (!value) {
      return '0 B';
    }
    const units = ['B', 'KB', 'MB', 'GB'];
    let size = value;
    let unit = 0;
    while (size >= 1024 && unit < units.length - 1) {
      size /= 1024;
      unit += 1;
    }
    return `${size.toFixed(size >= 10 || unit === 0 ? 0 : 1)} ${units[unit]}`;
  }

  function installHash(app: RegistryApp): string {
    return app.latestVersionHash || app.id;
  }

  function appRef(app: RegistryApp): string {
    return `${app.ownerId}/${app.name}@${installHash(app)}`;
  }

  function escapedODataId(value: string): string {
    return value.replace(/'/g, "''");
  }

  function odataInstallCommand(app: RegistryApp): string {
    const ref = appRef(app);
    const body = JSON.stringify({
      TargetTenant: 'default',
      AppRef: ref,
      Installer: 'manual'
    });
    return `curl -sS -X POST "${location.origin}/tdata/Apps('${escapedODataId(app.id)}')/App.Install" -H "Content-Type: application/json" -H "X-Tenant-Id: default" -d '${body}'`;
  }

  function cliInstallCommand(app: RegistryApp): string {
    return `temper install ${appRef(app)} --tenant default --url ${location.origin}`;
  }

  function temperPawInstallCommand(app: RegistryApp): string {
    return `install_app({"source":"genesis","app_ref":"${appRef(app)}","tenant":"default","url":"${location.origin}"})`;
  }

  function cloneCommand(app: RegistryApp): string {
    return `git clone ${location.origin}/${app.ownerId}/${app.name}.git`;
  }

  function closureEntries(closure: Closure): Array<[string, string]> {
    return parseJsonMap(closure.resolved);
  }

  function entriesForPath(entries: RepositoryFile[], path: string): RepositoryFile[] {
    return entries
      .filter((entry) => entry.parentPath === path)
      .sort((a, b) => {
        if (a.kind === 'directory' && b.kind !== 'directory') {
          return -1;
        }
        if (a.kind !== 'directory' && b.kind === 'directory') {
          return 1;
        }
        return a.name.localeCompare(b.name);
      });
  }

  function initialBrowserPath(entries: RepositoryFile[]): string {
    const rootEntries = entriesForPath(entries, '');
    const rootFiles = rootEntries.filter((entry) => entry.kind !== 'directory');
    const rootDirectories = rootEntries.filter((entry) => entry.kind === 'directory');
    if (rootFiles.length === 0 && rootDirectories.length === 1) {
      return rootDirectories[0].path;
    }
    return '';
  }

  function breadcrumbs(path: string): Array<{ label: string; path: string }> {
    const parts = path.split('/').filter(Boolean);
    const crumbs = [{ label: 'root', path: '' }];
    let cursor = '';
    for (const part of parts) {
      cursor = cursor ? `${cursor}/${part}` : part;
      crumbs.push({ label: part, path: cursor });
    }
    return crumbs;
  }

  function fileKindLabel(entry: RepositoryFile): string {
    if (entry.kind === 'directory') {
      return 'Directory';
    }
    if (entry.kind === 'symlink') {
      return 'Symlink';
    }
    if (entry.kind === 'submodule') {
      return 'Submodule';
    }
    return 'File';
  }
</script>

<svelte:head>
  <title>Genesis Registry</title>
  <meta
    name="description"
    content="Browse Genesis apps, repository files, lineage, dependency closures, and pinned install commands."
  />
</svelte:head>

<main class="shell">
  <header class="topbar">
    <div class="brand">
      <div class="brand-mark">G</div>
      <div>
        <h1>Genesis Registry</h1>
        <p>{apps.length} apps · {lineages.length} lineage links · {closures.length} closures</p>
      </div>
    </div>
    <div class="top-actions">
      <span class="chip {loading ? 'amber' : 'green'}">
        {loading ? 'Loading' : 'Live'}
      </span>
      <button class="icon-button" aria-label="Refresh registry data" on:click={refresh} disabled={loading}>
        {#if loading}
          <Loader2 size={17} />
        {:else}
          <RefreshCw size={17} />
        {/if}
      </button>
    </div>
  </header>

  <div class="workspace">
    <aside class="rail">
      <div class="section-head">
        <div class="section-title">
          <h2>Apps</h2>
          <p>Registry catalog</p>
        </div>
        <Boxes size={18} />
      </div>
      <div class="toolbar">
        <label class="search-field" aria-label="Search apps">
          <Search size={16} />
          <input bind:value={search} placeholder="Search apps" />
        </label>
        <select bind:value={statusFilter} aria-label="Status filter">
          <option value="all">All</option>
          <option value="Active">Active</option>
          <option value="Deprecated">Deprecated</option>
          <option value="Deleted">Deleted</option>
        </select>
      </div>

      {#if loadError}
        <div class="side-body">
          <div class="notice error">
            <AlertCircle size={16} />
            <span>{loadError}</span>
          </div>
        </div>
      {:else if filteredApps.length}
        <div class="app-list">
          {#each filteredApps as app}
            <button
              class:selected={selectedApp?.id === app.id}
              class="app-row"
              type="button"
              on:click={() => selectApp(app)}
            >
              <span class="app-icon">{appInitial(app)}</span>
              <span class="app-row-main">
                <strong>{app.name}</strong>
                <span>{app.ownerId}/{app.repositoryId}</span>
                <span class="row-meta">
                  <span class="chip {statusClass(app.status)}">{app.status}</span>
                  <span class="chip">{app.visibility}</span>
                </span>
              </span>
            </button>
          {/each}
        </div>
      {:else}
        <div class="empty compact-empty">
          <PackageCheck size={28} />
          <h3>No apps</h3>
          <p>{loading ? 'Loading registry rows.' : 'No App rows matched the current filter.'}</p>
        </div>
      {/if}
    </aside>

    <section class="main-stack">
      {#if selectedApp}
        <article class="panel app-workbench">
          <div class="detail-hero">
            <div class="identity">
              <div class="app-icon">{appInitial(selectedApp)}</div>
              <div class="identity-copy">
                <div class="chips">
                  <span class="chip {statusClass(selectedApp.status)}">{selectedApp.status}</span>
                  <span class="chip">{selectedApp.visibility}</span>
                  {#if selectedLineage}
                    <span class="chip green"><GitFork size={13} /> {selectedLineage.type}</span>
                  {/if}
                </div>
                <h2>{selectedApp.name}</h2>
                <p>{selectedApp.description || `${selectedApp.ownerId}/${selectedApp.repositoryId}`}</p>
              </div>
            </div>
            <button
              class="action-button"
              type="button"
              on:click={() => copyText(cloneCommand(selectedApp), 'Clone command')}
            >
              <Copy size={16} />
              Copy Clone
            </button>
          </div>

          <nav class="tabs" aria-label="App sections">
            <button
              class:active={activeTab === 'files'}
              type="button"
              on:click={() => selectTab('files')}
            >
              <Files size={15} /> Files
            </button>
            <button
              class:active={activeTab === 'overview'}
              type="button"
              on:click={() => selectTab('overview')}
            >
              <Clipboard size={15} /> Overview
            </button>
            <button
              class:active={activeTab === 'lineage'}
              type="button"
              on:click={() => selectTab('lineage')}
            >
              <GitBranch size={15} /> Lineage
            </button>
            <button
              class:active={activeTab === 'install'}
              type="button"
              on:click={() => selectTab('install')}
            >
              <PackageCheck size={15} /> Install
            </button>
          </nav>

          <div class="tab-panel">
            {#if activeTab === 'files'}
              <section class="files-view">
                <div class="repo-strip">
                  <div>
                    <span>Commit</span>
                    <strong>{shortHash(fileSnapshot?.commit?.id ?? selectedApp.latestVersionHash, 16)}</strong>
                    <p>{fileSnapshot?.commit?.message?.trim() || 'No commit message loaded.'}</p>
                  </div>
                  <div>
                    <span>Contents</span>
                    <strong>{fileCount} files · {directoryCount} folders</strong>
                    <p>{formatBytes(repositorySize)} stored in blobs</p>
                  </div>
                </div>

                <div class="file-layout">
                  <section class="file-browser">
                    <div class="breadcrumb" aria-label="Repository path">
                      {#each currentBreadcrumbs as crumb, index}
                        {#if index > 0}
                          <span>/</span>
                        {/if}
                        <button type="button" on:click={() => (currentPath = crumb.path)}>
                          {crumb.label}
                        </button>
                      {/each}
                    </div>

                    {#if filesLoading}
                      <div class="empty">
                        <Loader2 size={28} />
                        <h3>Loading files</h3>
                        <p>Reading commit, tree, and blob rows from Temper.</p>
                      </div>
                    {:else if filesError}
                      <div class="notice error">
                        <AlertCircle size={16} />
                        <span>{filesError}</span>
                      </div>
                    {:else if !fileEntries.length}
                      <div class="empty">
                        <Files size={30} />
                        <h3>No files projected</h3>
                        <p>This app has no loaded commit tree yet.</p>
                      </div>
                    {:else}
                      <div class="file-table" role="table" aria-label="Repository files">
                        {#each visibleEntries as entry}
                          <button
                            class:selected={selectedFilePath === entry.path}
                            class="file-row"
                            type="button"
                            on:click={() => selectEntry(entry)}
                          >
                            <span class="file-name">
                              {#if entry.kind === 'directory'}
                                <Folder size={16} />
                              {:else}
                                <File size={16} />
                              {/if}
                              <strong>{entry.name}</strong>
                            </span>
                            <span>{fileKindLabel(entry)}</span>
                            <code>{shortHash(entry.objectSha, 10)}</code>
                            <span>{entry.kind === 'directory' ? '' : formatBytes(entry.size)}</span>
                          </button>
                        {/each}
                      </div>
                    {/if}
                  </section>

                  <aside class="file-preview">
                    {#if selectedFile}
                      <div class="preview-head">
                        <div>
                          <span>{selectedFile.mode}</span>
                          <h3>{selectedFile.path}</h3>
                        </div>
                        <span class="chip">{formatBytes(selectedFile.size)}</span>
                      </div>
                      {#if selectedFile.isBinary}
                        <div class="empty compact-empty">
                          <File size={28} />
                          <h3>Binary file</h3>
                          <p>Preview is unavailable for this blob.</p>
                        </div>
                      {:else}
                        <pre class="code-preview">{selectedFile.preview || 'Empty file'}</pre>
                      {/if}
                    {:else}
                      <div class="empty compact-empty">
                        <ListTree size={30} />
                        <h3>Select a file</h3>
                        <p>Open a directory or choose a blob to inspect its contents.</p>
                      </div>
                    {/if}
                  </aside>
                </div>
              </section>
            {:else if activeTab === 'overview'}
              <section class="overview-view">
                <div class="metric-grid">
                  <div class="metric">
                    <span>Owner</span>
                    <strong>{ownerLabel(selectedApp.ownerId)}</strong>
                  </div>
                  <div class="metric">
                    <span>Repository</span>
                    <strong>{selectedApp.repositoryId}</strong>
                  </div>
                  <div class="metric">
                    <span>Latest Hash</span>
                    <strong>{shortHash(selectedApp.latestVersionHash)}</strong>
                  </div>
                  <div class="metric">
                    <span>Updated</span>
                    <strong>{displayDate(selectedApp.updatedAt || selectedApp.createdAt)}</strong>
                  </div>
                </div>

                <div class="split">
                  <section>
                    <div class="subhead">
                      <h3>App Detail</h3>
                      <Clipboard size={16} />
                    </div>
                    <div class="kv">
                      <span>App ID</span>
                      <code>{selectedApp.id}</code>
                      <span>Owner ID</span>
                      <strong>{selectedApp.ownerId}</strong>
                      <span>Exports</span>
                      <strong>{exportsList.length ? `${exportsList.length} entries` : 'none recorded'}</strong>
                      <span>Created</span>
                      <strong>{displayDate(selectedApp.createdAt)}</strong>
                    </div>
                    {#if exportsList.length}
                      <div class="chips inline-chips">
                        {#each exportsList as item}
                          <span class="chip">{item}</span>
                        {/each}
                      </div>
                    {/if}
                  </section>

                  <section>
                    <div class="subhead">
                      <h3>Closures</h3>
                      <Boxes size={16} />
                    </div>
                    {#if selectedClosures.length}
                      <div class="closure-list">
                        {#each selectedClosures as closure}
                          <div class="closure-row">
                            <strong>{closure.id}</strong>
                            <span>{closure.resolverVersion} · {displayDate(closure.resolvedAt)}</span>
                            {#each closureEntries(closure).slice(0, 3) as [name, hash]}
                              <code>{name}: {shortHash(hash, 16)}</code>
                            {/each}
                          </div>
                        {/each}
                      </div>
                    {:else}
                      <div class="notice">
                        <Boxes size={16} />
                        <span>No closure rows matched the selected app.</span>
                      </div>
                    {/if}
                  </section>
                </div>
              </section>
            {:else if activeTab === 'lineage'}
              <section class="lineage-view">
                <div class="graph-wrap">
                  <svg class="lineage-svg" viewBox="0 0 760 260" role="img" aria-label="Lineage graph">
                    <defs>
                      <marker id="arrow" markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto">
                        <path d="M0,0 L0,6 L7,3 z" fill="#8b9585" />
                      </marker>
                    </defs>

                    {#if parentApp}
                      <line x1="206" y1="130" x2="318" y2="130" stroke="#8b9585" stroke-width="2" marker-end="url(#arrow)" />
                    {/if}
                    {#each childApps.slice(0, 2) as child, index}
                      <line
                        x1="460"
                        y1="130"
                        x2="552"
                        y2={index === 0 ? 88 : 172}
                        stroke="#8b9585"
                        stroke-width="2"
                        marker-end="url(#arrow)"
                      />
                    {/each}

                    {#if parentApp}
                      <g transform="translate(34 96)">
                        <rect width="172" height="68" rx="8" fill="#fff8e5" stroke="#e6ca8b" />
                        <text class="svg-label" x="14" y="28">{parentApp.name}</text>
                        <text class="svg-meta" x="14" y="48">{shortHash(selectedLineage?.parentCommit ?? parentApp.latestVersionHash, 18)}</text>
                      </g>
                    {/if}

                    <g transform="translate(318 88)">
                      <rect width="142" height="84" rx="8" fill="#ecf8f5" stroke="#176f67" />
                      <text class="svg-label" x="14" y="30">{selectedApp.name}</text>
                      <text class="svg-meta" x="14" y="52">{selectedApp.ownerId}</text>
                      <text class="svg-meta" x="14" y="68">{shortHash(selectedApp.latestVersionHash, 18)}</text>
                    </g>

                    {#each childApps.slice(0, 2) as child, index}
                      <g transform={`translate(552 ${index === 0 ? 54 : 138})`}>
                        <rect width="172" height="68" rx="8" fill="#ffffff" stroke="#d7dcd0" />
                        <text class="svg-label" x="14" y="28">{child.name}</text>
                        <text class="svg-meta" x="14" y="48">{child.ownerId}</text>
                      </g>
                    {/each}

                    {#if !parentApp && childApps.length === 0}
                      <text class="svg-meta" x="310" y="204">No fork links recorded</text>
                    {/if}
                  </svg>
                </div>

                <div class="compare-grid">
                  <div class="compare-box">
                    <span>Parent</span>
                    {#if parentApp}
                      <strong>{parentApp.ownerId}/{parentApp.name}</strong>
                      <code>{parentApp.repositoryId}</code>
                    {:else}
                      <strong>no parent</strong>
                      <code>{selectedLineage?.parentRepositoryId || 'root app'}</code>
                    {/if}
                  </div>
                  <div class="compare-box">
                    <span>Selected</span>
                    <strong>{selectedApp.ownerId}/{selectedApp.name}</strong>
                    <code>{selectedApp.repositoryId}</code>
                  </div>
                </div>

                <div class="mutations">
                  {#if mutationList.length}
                    <ul class="mutation-list">
                      {#each mutationList as mutation}
                        <li>{mutation}</li>
                      {/each}
                    </ul>
                  {:else}
                    <div class="notice">
                      <GitFork size={16} />
                      <span>No mutation records are attached to this lineage row.</span>
                    </div>
                  {/if}
                </div>
              </section>
            {:else if activeTab === 'install'}
              <section class="install-view">
                <div class="install-grid">
                  <div class="install-card">
                    <div class="subhead">
                      <h3>OData Action</h3>
                      <PackageCheck size={16} />
                    </div>
                    <div class="code-line">
                      <code>{odataInstallCommand(selectedApp)}</code>
                      <button
                        class="mini-button"
                        type="button"
                        aria-label="Copy OData install command"
                        on:click={() => copyText(odataInstallCommand(selectedApp), 'OData install command')}
                      >
                        <Copy size={14} />
                      </button>
                    </div>
                    <p class="muted-copy">Spec-owned install surface for pinned Genesis app bytes.</p>
                  </div>

                  <div class="install-card">
                    <div class="subhead">
                      <h3>Temper CLI</h3>
                      <PackageCheck size={16} />
                    </div>
                    <div class="code-line">
                      <code>{cliInstallCommand(selectedApp)}</code>
                      <button
                        class="mini-button"
                        type="button"
                        aria-label="Copy Temper CLI install command"
                        on:click={() => copyText(cliInstallCommand(selectedApp), 'Temper CLI install command')}
                      >
                        <Copy size={14} />
                      </button>
                    </div>
                    <p class="muted-copy">CLI wrapper around the same App.Install OData action.</p>
                  </div>

                  <div class="install-card">
                    <div class="subhead">
                      <h3>TemperPaw Tool</h3>
                      <PackageCheck size={16} />
                    </div>
                    <div class="code-line">
                      <code>{temperPawInstallCommand(selectedApp)}</code>
                      <button
                        class="mini-button"
                        type="button"
                        aria-label="Copy TemperPaw tool install call"
                        on:click={() => copyText(temperPawInstallCommand(selectedApp), 'TemperPaw install call')}
                      >
                        <Copy size={14} />
                      </button>
                    </div>
                    <p class="muted-copy">Tool path for an agent to request the same pinned app install.</p>
                  </div>

                  <div class="install-card">
                    <div class="subhead">
                      <h3>Clone</h3>
                      <GitCommitHorizontal size={16} />
                    </div>
                    <div class="code-line">
                      <code>{cloneCommand(selectedApp)}</code>
                      <button
                        class="mini-button"
                        type="button"
                        aria-label="Copy clone command"
                        on:click={() => copyText(cloneCommand(selectedApp), 'Clone command')}
                      >
                        <Copy size={14} />
                      </button>
                    </div>
                    <p class="muted-copy">Smart HTTP reconstructs this repository from Temper objects.</p>
                  </div>
                </div>

                {#if warnings.length}
                  <div class="warning-stack">
                    {#each warnings as warning}
                      <div class="notice error">
                        <AlertCircle size={16} />
                        <span>{warning}</span>
                      </div>
                    {/each}
                  </div>
                {/if}
              </section>
            {/if}
          </div>
        </article>
      {:else}
        <article class="panel">
          <div class="empty">
            <PackageCheck size={34} />
            <h2>Registry Empty</h2>
            <p>The UI is connected, but there are no App rows to render.</p>
          </div>
        </article>
      {/if}
    </section>
  </div>

  {#if toast}
    <div class="toast">{toast}</div>
  {/if}
</main>
