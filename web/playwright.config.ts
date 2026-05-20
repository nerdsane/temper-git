import { defineConfig, devices } from '@playwright/test';

const PORT = Number(process.env.GENESIS_WEB_TEST_PORT ?? 4174);
const host = `http://127.0.0.1:${PORT}`;

export default defineConfig({
  testDir: './tests/e2e',
  timeout: 30_000,
  expect: {
    timeout: 7_500
  },
  fullyParallel: true,
  reporter: process.env.CI ? [['dot'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: `${host}/genesis`,
    trace: 'on-first-retry'
  },
  projects: [
    {
      name: 'chromium-desktop',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 980 }
      }
    },
    {
      name: 'chromium-mobile',
      use: {
        ...devices['Pixel 7'],
        viewport: { width: 390, height: 844 }
      }
    }
  ],
  webServer: {
    command: `npm run dev -- --host 127.0.0.1 --port ${PORT}`,
    url: `${host}/genesis`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000
  }
});
