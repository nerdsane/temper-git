import { expect, type Page, test } from '@playwright/test';

type EntityRow = {
  entity_id: string;
  status: string;
  fields: Record<string, unknown>;
};

const parentHash = '1111111111111111111111111111111111111111';
const childHash = '2222222222222222222222222222222222222222';
const parentTreeHash = '3333333333333333333333333333333333333333';
const childTreeHash = '4444444444444444444444444444444444444444';
const readmeBlobHash = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const manifestBlobHash = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';

function row(entityType: string, id: string, status: string, fields: Record<string, unknown>): EntityRow {
  return {
    entity_id: id,
    status,
    fields: {
      Id: id,
      Status: status,
      ...fields
    },
    entity_type: entityType
  } as EntityRow;
}

const apps = [
  row('App', 'app-kernel-core', 'Active', {
    OwnerId: 'team',
    Name: 'kernel-core',
    RepositoryId: 'rp-team-kernel-core',
    LatestVersionHash: parentHash,
    Exports: JSON.stringify(['Repository.IngestPack', 'App.Fork']),
    Description: 'Spec-first kernel primitives',
    Visibility: 'public',
    CreatedAt: '2026-05-19T08:00:00Z',
    UpdatedAt: '2026-05-19T08:01:00Z'
  }),
  row('App', 'app-alice-notes', 'Active', {
    OwnerId: 'alice',
    Name: 'alice-notes',
    RepositoryId: 'rp-alice-notes',
    LatestVersionHash: childHash,
    Exports: JSON.stringify(['notes.view']),
    Description: 'Forked notes workspace',
    Visibility: 'public',
    CreatedAt: '2026-05-19T08:02:00Z',
    UpdatedAt: '2026-05-19T08:03:00Z'
  })
];

const owners = [
  row('Owner', 'team', 'Verified', {
    AccountId: 'team',
    DisplayName: 'Temper Team',
    Contact: 'ops@example.test',
    StorageCapBytes: 104_857_600,
    RateLimitTier: 'pro',
    VerificationProvider: 'oauth',
    VerificationSubject: 'github:temper-team',
    VerifiedAt: '2026-05-19T08:00:00Z'
  }),
  row('Owner', 'alice', 'PendingVerification', {
    AccountId: 'alice',
    DisplayName: 'Alice',
    Contact: 'alice@example.test',
    StorageCapBytes: 104_857_600,
    RateLimitTier: 'free',
    VerificationProvider: 'email_magic_link',
    VerificationSubject: 'alice@example.test'
  })
];

const lineages = [
  row('Lineage', 'ln-alice-notes', 'Active', {
    ChildRepositoryId: 'rp-alice-notes',
    ParentRepositoryId: 'rp-team-kernel-core',
    ParentCommit: parentHash,
    Type: 'fork',
    CreatedBy: 'alice',
    Mutations: JSON.stringify(['rename README.md', 'add notes route']),
    CreatedAt: '2026-05-19T08:04:00Z'
  })
];

const closures = [
  row('Closure', 'cl-test-realpack', 'Durable', {
    Root: 'app-alice-notes',
    Resolved: JSON.stringify({
      'kernel-core': `@${parentHash}`,
      'alice-notes': `@${childHash}`
    }),
    ResolverVersion: '1.0',
    ResolvedAt: '2026-05-19T08:05:00Z',
    ResolvedBy: 'playwright-regression'
  })
];

function base64(value: string): string {
  return Buffer.from(value, 'utf8').toString('base64');
}

function treeCanonical(entries: Array<{ mode: string; name: string; sha: string }>): string {
  const body = entries.flatMap((entry) => [
    ...Array.from(Buffer.from(`${entry.mode} ${entry.name}\0`, 'utf8')),
    ...Array.from(Buffer.from(entry.sha, 'hex'))
  ]);
  const header = Array.from(Buffer.from(`tree ${body.length}\0`, 'utf8'));
  return Buffer.from([...header, ...body]).toString('base64');
}

const commits = [
  row('Commit', parentHash, 'Durable', {
    RepositoryId: 'rp-team-kernel-core',
    TreeSha: parentTreeHash,
    ParentShas: '',
    Author: 'Team <team@example.test>',
    Committer: 'Team <team@example.test>',
    Message: 'parent registry commit\n',
    CreatedAt: '2026-05-19T08:00:00Z'
  }),
  row('Commit', childHash, 'Durable', {
    RepositoryId: 'rp-alice-notes',
    TreeSha: childTreeHash,
    ParentShas: parentHash,
    Author: 'Alice <alice@example.test>',
    Committer: 'Alice <alice@example.test>',
    Message: 'add notes app\n',
    CreatedAt: '2026-05-19T08:04:00Z'
  })
];

const trees = [
  row('Tree', parentTreeHash, 'Durable', {
    RepositoryId: 'rp-team-kernel-core',
    CanonicalBytes: treeCanonical([
      { mode: '100644', name: 'README.md', sha: readmeBlobHash }
    ])
  }),
  row('Tree', childTreeHash, 'Durable', {
    RepositoryId: 'rp-alice-notes',
    CanonicalBytes: treeCanonical([
      { mode: '100644', name: 'README.md', sha: readmeBlobHash },
      { mode: '100644', name: 'app.toml', sha: manifestBlobHash }
    ])
  })
];

const blobs = [
  row('Blob', readmeBlobHash, 'Durable', {
    RepositoryId: 'rp-alice-notes',
    Content: base64('# Alice Notes\n'),
    Size: 14
  }),
  row('Blob', manifestBlobHash, 'Durable', {
    RepositoryId: 'rp-alice-notes',
    Content: base64('name = "alice-notes"\n'),
    Size: 21
  })
];

async function mockOData(page: Page) {
  await page.route('**/tdata/Apps', async (route) => {
    await route.fulfill({ json: { value: apps } });
  });
  await page.route('**/tdata/Lineages', async (route) => {
    await route.fulfill({ json: { value: lineages } });
  });
  await page.route('**/tdata/Closures', async (route) => {
    await route.fulfill({ json: { value: closures } });
  });
  await page.route('**/tdata/Owners', async (route) => {
    if (route.request().method() === 'POST') {
      const body = route.request().postDataJSON() as Record<string, unknown>;
      if (
        body.Id === 'newco' &&
        (body.VerificationProvider !== 'oauth' ||
          body.VerificationSubject !== 'github:newco')
      ) {
        await route.fulfill({
          status: 400,
          json: {
            error: {
              message: `unexpected verification payload ${JSON.stringify(body)}`
            }
          }
        });
        return;
      }
      await route.fulfill({
        status: 201,
        json: row('Owner', String(body.Id), 'PendingVerification', body)
      });
      return;
    }
    await route.fulfill({ json: { value: owners } });
  });
  await page.route('**/tdata/Commits*', async (route) => {
    await route.fulfill({ json: { value: commits } });
  });
  await page.route('**/tdata/Trees*', async (route) => {
    await route.fulfill({ json: { value: trees } });
  });
  await page.route('**/tdata/Blobs*', async (route) => {
    await route.fulfill({ json: { value: blobs } });
  });
}

test.beforeEach(async ({ page }) => {
  await mockOData(page);
});

test('renders browse, lineage, closures, and Genesis install surfaces without browser errors', async ({
  page
}) => {
  const browserErrors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') {
      browserErrors.push(message.text());
    }
  });
  page.on('pageerror', (error) => browserErrors.push(error.message));

  await page.goto('/');

  await expect(page.getByRole('heading', { name: 'Genesis Registry' })).toBeVisible();
  await expect(page.getByText('2 apps · 1 lineage links · 1 closures')).toBeVisible();
  await expect(page.getByRole('button', { name: /alice-notes/ })).toBeVisible();
  await page.getByRole('button', { name: /alice-notes/ }).click();

  await expect(page.getByRole('heading', { name: 'alice-notes' })).toBeVisible();
  await expect(page.getByRole('button', { name: /app.toml/ })).toBeVisible();
  await page.getByRole('button', { name: /app.toml/ }).click();
  await expect(page.getByText('name = "alice-notes"')).toBeVisible();

  await page.getByRole('button', { name: 'Overview' }).click();
  await expect(page.getByText('Alice', { exact: true })).toBeVisible();
  await expect(page.getByText('cl-test-realpack')).toBeVisible();
  await expect(page.getByText(/kernel-core:/)).toBeVisible();

  await page.getByRole('button', { name: 'Lineage' }).click();
  await expect(page.getByText('team/kernel-core')).toBeVisible();
  await expect(page.getByText('alice/alice-notes', { exact: true })).toBeVisible();
  await expect(page.getByLabel('Lineage graph')).toBeVisible();

  await page.getByRole('button', { name: 'Install' }).click();
  await expect(
    page.getByText(`/tdata/Apps('app-alice-notes')/App.Install`)
  ).toBeVisible();
  await expect(
    page.getByText(`temper install alice/alice-notes@${childHash} --tenant default --url`)
  ).toBeVisible();
  await expect(
    page.getByText(`install_app({"source":"genesis","app_ref":"alice/alice-notes@${childHash}"`)
  ).toBeVisible();
  await expect(page.getByText('git clone')).toBeVisible();

  await page.getByPlaceholder('Search apps').fill('kernel');
  await expect(page.getByRole('button', { name: /kernel-core/ })).toBeVisible();
  await expect(page.getByRole('button', { name: /alice-notes/ })).toHaveCount(0);
  await page.getByPlaceholder('Search apps').fill('');
  await expect(page.getByRole('button', { name: 'Account' })).toHaveCount(0);
  await expect(page.getByText('Claim Namespace')).toHaveCount(0);

  const horizontalOverflow = await page.evaluate(() => {
    const root = document.documentElement;
    return root.scrollWidth - root.clientWidth;
  });
  expect(horizontalOverflow).toBeLessThanOrEqual(1);
  expect(browserErrors).toEqual([]);
});
