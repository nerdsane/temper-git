export type EntityRow = Record<string, unknown> & {
  fields?: Record<string, unknown>;
  entity_id?: string;
  status?: string;
};

export type LoadWarning = {
  collection: string;
  message: string;
};

export type RegistryApp = {
  id: string;
  ownerId: string;
  name: string;
  repositoryId: string;
  latestVersionHash: string;
  exports: string;
  description: string;
  visibility: string;
  status: string;
  createdAt: string;
  updatedAt: string;
  raw: EntityRow;
};

export type GitCommit = {
  id: string;
  repositoryId: string;
  treeSha: string;
  parentShas: string;
  author: string;
  committer: string;
  message: string;
  createdAt: string;
  raw: EntityRow;
};

export type GitTree = {
  id: string;
  repositoryId: string;
  canonicalBytes: string;
  raw: EntityRow;
};

export type GitBlob = {
  id: string;
  repositoryId: string;
  content: string;
  size: number;
  createdAt: string;
  raw: EntityRow;
};

export type RepositoryFile = {
  path: string;
  name: string;
  parentPath: string;
  kind: 'directory' | 'file' | 'symlink' | 'submodule';
  mode: string;
  objectSha: string;
  size: number;
  preview: string;
  isBinary: boolean;
};

export type AppFilesSnapshot = {
  appId: string;
  repositoryId: string;
  commitHash: string;
  commit: GitCommit | null;
  files: RepositoryFile[];
};

export type Owner = {
  id: string;
  accountId: string;
  displayName: string;
  contact: string;
  storageCapBytes: number;
  rateLimitTier: string;
  verificationProvider: string;
  verificationSubject: string;
  verifiedAt: string;
  status: string;
  raw: EntityRow;
};

export type Lineage = {
  id: string;
  childRepositoryId: string;
  parentRepositoryId: string;
  parentCommit: string;
  type: string;
  createdBy: string;
  mutations: string;
  status: string;
  createdAt: string;
  raw: EntityRow;
};

export type Closure = {
  id: string;
  root: string;
  resolved: string;
  resolverVersion: string;
  resolvedAt: string;
  resolvedBy: string;
  status: string;
  raw: EntityRow;
};

export type RegistrySnapshot = {
  apps: RegistryApp[];
  owners: Owner[];
  lineages: Lineage[];
  closures: Closure[];
  warnings: LoadWarning[];
};

export type ClaimOwnerInput = {
  accountId: string;
  displayName: string;
  contact: string;
  verificationProvider: string;
  verificationSubject: string;
};
