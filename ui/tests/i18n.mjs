import {selectOption, expectOptions} from './helpers/select.mjs';
import assert from 'node:assert/strict';
import {chromium, expect} from '@playwright/test';
import {staticUi} from './helpers/static-ui.mjs';
import {demoId, demoMedia, telemetryFixture} from './helpers/telemetry-fixtures.mjs';
import {emptyThroughputHistory} from './helpers/throughput-history-fixtures.mjs';

const ui = await staticUi();
let browser;
const writes = [], errors = [], deletionPreviews = [];
try {
  browser = await chromium.launch({executablePath: process.env.COLD_VTL_CHROMIUM || undefined, args: ['--no-sandbox']});
  // A fresh English browser has no stored language preference.
  const context = await browser.newContext({locale: 'en-GB', colorScheme: 'light', viewport: {width: 1440, height: 1000}});
  const page = await context.newPage();
  page.on('pageerror', error => errors.push(error.message));
  let role = 'admin', driveLabel = 'Lecteur 1';
  await context.route('**/auth/**', route => {
    const path = new URL(route.request().url()).pathname;
    if (route.request().method() !== 'GET') {
      writes.push(path);
      return route.fulfill({status: 409, json: {error: 'Unexpected mutation in read-only smoke'}});
    }
    return route.fulfill({json: /\/auth\/(tokens|users)$/.test(path) ? [] : {
      local: true, oidc: false,
      user: {id: 'i18n-admin', username: 'Alice', role, issuer: null, disabled: false}, csrf_token: 'i18n-test',
    }});
  });
  await context.route('**/v1/**', route => {
    const path = new URL(route.request().url()).pathname;
    // This POST only previews catalogue references; never permit delete-local.
    if (path.endsWith('/cartridges/deletion') && route.request().method() === 'POST') {
      const selection = route.request().postDataJSON();
      deletionPreviews.push(selection);
      return route.fulfill({json: {ready: false, reason: null, pending: null, cartridges: selection.barcodes.map(barcode => ({barcode, exports: []}))}});
    }
    if (route.request().method() !== 'GET') {
      writes.push(path);
      return route.fulfill({status: 409, json: {error: 'Unexpected mutation in read-only smoke'}});
    }
    if (path === '/v1/vtls') return route.fulfill({json: [{id: demoId, name: 'Archives Équipe A', lifecycle: 'ready', provisioning_revision: 1, storage: null}]});
    if (path.endsWith('/telemetry')) {
      // Fresh timestamps, unchanged activity: only real state changes may add journal entries.
      const value = telemetryFixture(demoId, 'writing');
      value.drives[0].label = driveLabel;
      value.drives[0].write_rate_bps = 270 * 1024 ** 2;
      return route.fulfill({json: value});
    }
    if (path.endsWith('/telemetry/history')) return route.fulfill({json: emptyThroughputHistory(demoId, route.request().url())});
    if (path.endsWith('/maintenance')) return route.fulfill({json: {configured: true, phase: 'online', ready: false, blocked: false, services: {'tgt.service': 'active', 'vtltape@11.service': 'active'}}});
    if (path.endsWith('/storage/connection')) return route.fulfill({json: {revision: 0, configured: false, secret_configured: false, configuration: null}});
    if (path.endsWith('/settings/tape-storage')) return route.fulfill({json: {vtl_id: demoId, mode: 'managed', phase: 'ready', revision: 1, path: '/srv/tapes', capacity: null, operation: null, message: null, data_locked: false, change_allowed: true, change_lock_reason: null}});
    if (path.endsWith('/settings/s3')) return route.fulfill({json: {providers: [], configured: false, revision: 0, secret_configured: false, configuration: null, reconcile_required: 0}});
    if (path === '/v1/notifications/settings') return route.fulfill({json: {revision: 1, has_password: false, settings: {
      enabled: false, language: 'fr', installation: 'Archives Équipe A', host: 'smtp.example.test', port: 587, tls: 'starttls', username: '',
      sender: 'vaultcenter@example.test', sender_name: 'VaultCenter', recipients: ['ops@example.test'],
      events: Object.fromEntries(['export', 'restore', 'reintegrate', 'archive', 'recall'].map(kind => [kind, {success: true, failure: true}])),
    }}});
    if (path === '/v1/notifications/preview') return route.fulfill({json: {subject: '[VaultCenter] Export S3 terminé', html: '<html lang="fr"><body><h1>Export S3 terminé</h1></body></html>', text: 'Export S3 terminé'}});
    if (path === '/v1/notifications/deliveries') return route.fulfill({json: {items: [], has_more: false, offset: 0}});
    if (path.endsWith('/cartridges/options')) return route.fulfill({json: {
      configured: true, ready: true, online_creation: true, reason: null, pending_creation: null, pending_batch: null, free_slots: 7,
      media_types: ['LTO8'], default_media_type: 'LTO8', default_capacity_mib: 2048, default_segment_size_mib: 1048576,
      default_multipart_part_size_mib: 256, min_multipart_part_size_mib: 16, max_multipart_part_size_mib: 1024,
      min_capacity_mib: 1, max_capacity_mib: 16777216, min_segment_size_mib: 16, max_segment_size_mib: 1048576,
      storage: {free_bytes: 32 * 1024 ** 3, reserved_bytes: 0, available_bytes: 32 * 1024 ** 3, overhead_per_cartridge: 16 * 1024 ** 2},
    }});
    if (path.endsWith('/bandwidth')) return route.fulfill({json: {vtl_id: demoId, revision: 1, tape_write_bps: null, s3_bps: null, tape: {state: 'confirmed', observed_revision: 1}, s3: {state: 'configured'}, message: null}});
    if (path.endsWith('/cartridges/deletion')) return route.fulfill({json: {ready: false, pending: null, reason: null}});
    return route.fulfill({json: path.endsWith('/cartridges') ? demoMedia() : path.endsWith('/jobs') || path.endsWith('/maintenance/events') ? [] : {status: 'ok', cartridges: 9, jobs: 0}});
  });

  await page.goto(ui.url);
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await expect(page.getByRole('combobox', {name: 'Interface language', exact: true})).toHaveAttribute('data-value', 'en');
  const nav = () => page.getByRole('navigation', {name: 'Library navigation', exact: true});
  for (const label of ['Library', 'Activity', 'S3 storage', 'Settings']) await expect(nav().getByRole('link', {name: label, exact: true})).toBeVisible();
  await expect(page.getByRole('article', {name: 'Cartridge CVT001L8', exact: true})).toBeVisible();
  const status = () => page.getByRole('region', {name: 'Drive states', exact: true});
  await expect(status()).toContainText('Drive 1');
  await expect(status()).toContainText('Writing');
  await expect(status()).toContainText('270 MiB/s');
  await expect(page.locator('#vtl-selection')).toContainText('Archives Équipe A');

  const journal = page.locator('.lto-lcd-entry');
  await expect(journal).not.toHaveCount(0);
  const initialJournal = await journal.locator('time').evaluateAll(nodes => nodes.map(node => node.dateTime));
  await selectOption(page.getByRole('combobox', {name: 'Interface language', exact: true}), 'fr');
  await expect(page.locator('html')).toHaveAttribute('lang', 'fr');
  await expect(page.getByRole('region', {name: 'État des lecteurs', exact: true})).toContainText('270 Mio/s');
  await expect(page.getByRole('region', {name: 'État des lecteurs', exact: true})).toContainText('Lecteur 1');
  await selectOption(page.getByRole('combobox', {name: 'Langue de l’interface', exact: true}), 'en');
  await expect(status()).toContainText('Drive 1');
  assert.deepEqual(await journal.locator('time').evaluateAll(nodes => nodes.map(node => node.dateTime)), initialJournal, 'Language switches must not append or reset journal entries');

  await page.getByRole('button', {name: 'Create cartridges', exact: true}).click();
  const createDialog = page.getByRole('dialog', {name: 'Create cartridges', exact: true});
  await expect(createDialog.getByLabel('Number of cartridges', {exact: true})).toBeVisible();
  await expect(createDialog.getByRole('button', {name: 'Create 1 cartridge', exact: true})).toBeVisible();
  await createDialog.locator('form').getByRole('button', {name: 'Close', exact: true}).click();
  await expect(createDialog).toHaveCount(0);

  await page.getByRole('checkbox', {name: 'Select CVT001L8', exact: true}).check();
  await page.getByRole('button', {name: 'Delete (1)', exact: true}).click();
  const deleteDialog = page.getByRole('dialog', {name: 'Delete locally', exact: true});
  await expect(deleteDialog).toContainText('Before entering maintenance, wait for the backups to finish');
  await expect(deleteDialog.getByRole('button', {name: 'Enter maintenance', exact: true})).toBeVisible();
  await expect(deleteDialog.getByRole('button', {name: 'Delete', exact: true})).toBeDisabled();
  await expect(deleteDialog.getByRole('checkbox')).toHaveCount(0);
  await deleteDialog.getByRole('button', {name: 'Cancel', exact: true}).click();
  await expect(deleteDialog).toHaveCount(0);
  await expect(page.getByRole('checkbox', {name: 'Select CVT001L8', exact: true})).toBeChecked();
  await page.getByRole('checkbox', {name: 'Select CVT001L8', exact: true}).uncheck();

  await page.getByRole('button', {name: 'Maintenance', exact: true}).click();
  const maintenanceDialog = page.getByRole('dialog', {name: 'Maintenance', exact: true});
  await expect(maintenanceDialog).toContainText('Before entering maintenance, wait for the backups to finish and remove the cartridge from the drive in the backup software.');
  await expect(maintenanceDialog.getByRole('button', {name: 'Enter maintenance', exact: true})).toBeVisible();
  await maintenanceDialog.getByRole('button', {name: 'Close', exact: true}).click();
  await expect(maintenanceDialog).toHaveCount(0);

  // S3 setup lives in the library's Storage dialog; the S3 page only links to it.
  await nav().getByRole('link', {name: 'S3 storage', exact: true}).click();
  await expect(page.getByText('S3 storage not configured', {exact: true})).toBeVisible();
  await expect(page.getByRole('button', {name: 'Configuration', exact: true})).toHaveCount(0);
  await page.getByRole('button', {name: 'Configure storage for Archives Équipe A', exact: true}).click();
  const storageDialog = page.getByRole('dialog', {name: 'Archives Équipe A', exact: true});
  await expect(storageDialog.getByRole('button', {name: 'S3', exact: true})).toHaveAttribute('aria-pressed', 'true');
  await expect(storageDialog.getByRole('region', {name: 'S3 connection', exact: true})).toBeVisible();
  await expect(storageDialog.getByRole('region', {name: 'Export bucket', exact: true})).toContainText('Save the S3 connection first.');
  await storageDialog.getByText('Advanced options', {exact: true}).click();
  await expect(storageDialog.getByRole('region', {name: 'Use an existing bucket', exact: true})).toBeVisible();
  await expect(storageDialog.getByRole('button', {name: 'Use as active bucket', exact: true})).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(storageDialog).toHaveCount(0);

  await nav().getByRole('link', {name: 'Activity', exact: true}).click();
  await expect(page.getByText('0 operations recorded.', {exact: true})).toBeVisible();
  await selectOption(page.getByRole('combobox', {name: 'Interface language', exact: true}), 'fr');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('lang', 'fr');
  await expect(page.getByText('0 opération enregistrée.', {exact: true})).toBeVisible();
  await selectOption(page.getByRole('combobox', {name: 'Langue de l’interface', exact: true}), 'en');
  await page.reload();
  await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  await expect(page.getByText('0 operations recorded.', {exact: true})).toBeVisible();

  await nav().getByRole('link', {name: 'Settings', exact: true}).click();
  const sections = () => page.getByRole('navigation', {name: 'Settings sections', exact: true});
  for (const label of ['Notifications', 'Users', 'Profile']) await expect(sections().getByRole('link', {name: label, exact: true})).toBeVisible();
  await sections().getByRole('link', {name: 'Notifications', exact: true}).click();
  const emailLanguage = page.locator('#email-language');
  await expect(page.getByLabel('Email language', {exact: true})).toHaveAttribute('data-value', 'fr');
  await expectOptions(emailLanguage, ['Français', 'English']);
  await expect(page.getByText('This language applies to all emails when they are sent, independently of the interface language.', {exact: true})).toBeVisible();
  await selectOption(emailLanguage, 'en');
  await page.getByLabel('Installation name', {exact: true}).fill('Notification draft kept');
  await expect(page.getByText('Unsaved changes', {exact: true})).toBeVisible();
  await selectOption(page.getByRole('combobox', {name: 'Interface language', exact: true}), 'fr');
  await expect(page.getByLabel('Langue des emails', {exact: true})).toHaveAttribute('data-value', 'en');
  await expectOptions(emailLanguage, ['Français', 'English']);
  await expect(page.getByLabel('Nom de l’installation', {exact: true})).toHaveValue('Notification draft kept');
  await expect(page.getByText('Modifications non enregistrées', {exact: true})).toBeVisible();
  await selectOption(page.getByRole('combobox', {name: 'Langue de l’interface', exact: true}), 'en');
  await expect(page.getByLabel('Email language', {exact: true})).toHaveAttribute('data-value', 'en');
  await expect(page.getByLabel('Installation name', {exact: true})).toHaveValue('Notification draft kept');
  await nav().getByRole('link', {name: 'Library', exact: true}).click();
  driveLabel = 'Lecteur LTO · 01';
  await expect(status()).toContainText('LTO drive · 01', {timeout: 8000});
  driveLabel = 'Lecteur principal Alice';
  await expect(status()).toContainText('Lecteur principal Alice', {timeout: 8000});

  await page.setViewportSize({width: 390, height: 844});
  for (const theme of ['light', 'dark']) {
    if (!await page.locator('html').evaluate((node, value) => node.classList.contains(value), theme)) await page.getByRole('button', {name: 'Change theme', exact: true}).click();
    await expect(page.locator('html')).toHaveClass(new RegExp(`\\b${theme}\\b`));
    await expect(nav()).toBeVisible();
    await expect(status()).toBeInViewport();
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `${theme}: mobile page overflows horizontally`);
    await expect(page.getByRole('combobox', {name: 'Interface language', exact: true})).toBeVisible();
  }

  role = 'viewer';
  await page.goto(`${ui.url}/#notifications?vtl=${demoId}`);
  await page.reload();
  await expect(sections().getByRole('link', {name: 'Profile', exact: true})).toBeVisible();
  await expect(sections().getByRole('link', {name: 'Notifications', exact: true})).toHaveCount(0);
  await expect(sections().getByRole('link', {name: 'Users', exact: true})).toHaveCount(0);
  await nav().getByRole('link', {name: 'Library', exact: true}).click();
  await expect(page.getByRole('button', {name: 'Library power', exact: true})).toBeDisabled();
  await expect(page.getByRole('button', {name: 'Storage configuration', exact: true})).toHaveCount(0);
  await context.close();

  // Separate signed-out context: switch language without submitting credentials.
  const loginContext = await browser.newContext({locale: 'en-GB', viewport: {width: 390, height: 844}});
  const login = await loginContext.newPage();
  login.on('pageerror', error => errors.push(error.message));
  await loginContext.route('**/auth/**', route => {
    const path = new URL(route.request().url()).pathname;
    if (route.request().method() !== 'GET') {writes.push(path); return route.fulfill({status: 409, json: {error: 'Unexpected login submission'}});}
    return path === '/auth/me' ? route.fulfill({status: 401, json: {error: 'Authentication required.'}}) : route.fulfill({json: {local: true, oidc: false}});
  });
  await login.goto(ui.url);
  await expect(login.getByRole('heading', {name: 'Sign in to VaultCenter', exact: true})).toBeVisible();
  await login.getByLabel('Local user', {exact: true}).fill('draft-user');
  await login.getByLabel('Password', {exact: true}).fill('unsubmitted-password');
  await selectOption(login.getByRole('combobox', {name: 'Interface language', exact: true}), 'fr');
  await expect(login.getByLabel('Utilisateur local', {exact: true})).toHaveValue('draft-user');
  await expect(login.getByLabel('Mot de passe', {exact: true})).toHaveValue('unsubmitted-password');
  await expect(login.getByRole('button', {name: 'Se connecter', exact: true})).toBeVisible();
  await selectOption(login.getByRole('combobox', {name: 'Langue de l’interface', exact: true}), 'en');
  await expect(login.getByLabel('Local user', {exact: true})).toHaveValue('draft-user');
  await expect(login.getByLabel('Password', {exact: true})).toHaveValue('unsubmitted-password');
  assert.equal(await login.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  await loginContext.close();
  assert.deepEqual(writes, [], 'The locale smoke must never submit an operational request');
  assert.deepEqual(deletionPreviews, [{barcodes: ['CVT001L8']}], 'Only the selected cartridge may be previewed');
  assert.deepEqual(errors, []);
  console.log('PASS i18n: browser default, persisted FR/EN, S3/login/email drafts, independent email language, journal, labels/units/plurals, cancel-only dialogs, roles and mobile light/dark; no mutations.');
} finally {
  await browser?.close();
  await ui.close();
}
